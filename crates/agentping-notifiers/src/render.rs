//! Turns events into provider-neutral notifications using built-in or user templates.

use agentping_core::config::TemplateConfig;
use agentping_core::redact::redact;
use agentping_core::{Config, Event, EventType, Severity};
use chrono::{DateTime, Local, Utc};
use serde::Serialize;

/// A labelled value shown alongside the message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Field {
    /// Label.
    pub name: String,
    /// Value.
    pub value: String,
}

/// A provider-neutral notification ready to be formatted by a provider.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RenderedNotification {
    /// Event id (for correlation).
    pub event_id: String,
    /// Event type.
    pub event_type: EventType,
    /// Effective severity.
    pub severity: Severity,
    /// Short title, starting with an emoji.
    pub title: String,
    /// Body text (light Markdown).
    pub body: String,
    /// Labelled fields.
    pub fields: Vec<Field>,
    /// Footer text.
    pub footer: String,
    /// When the event happened.
    pub occurred_at: DateTime<Utc>,
    /// Project name, for compact push previews.
    pub project: Option<String>,
}

/// Emoji used for an event type.
pub fn emoji(event_type: &EventType, severity: Severity) -> &'static str {
    match event_type {
        EventType::PermissionRequest => "🔐",
        EventType::UserQuestion => "💬",
        EventType::TaskCompleted => "✅",
        EventType::TaskFailed => "⚠️",
        EventType::AgentIdle => "⏳",
        EventType::AgentStarted => "▶️",
        EventType::AgentStopped => "⏹️",
        EventType::Activity => "•",
        EventType::Notification | EventType::Custom(_) => {
            if severity >= Severity::High {
                "❗"
            } else {
                "🔔"
            }
        }
    }
}

fn default_title(event: &Event) -> String {
    let agent = &event.agent_name;
    match &event.event_type {
        EventType::PermissionRequest => format!("{agent} needs permission"),
        EventType::UserQuestion => format!("{agent} is asking a question"),
        EventType::TaskCompleted => format!("{agent} completed its task"),
        EventType::TaskFailed => format!("{agent} encountered an error"),
        EventType::AgentIdle => format!("{agent} is waiting for your input"),
        EventType::AgentStarted => format!("{agent} session started"),
        EventType::AgentStopped => format!("{agent} session ended"),
        EventType::Activity => format!("{agent} activity"),
        EventType::Notification | EventType::Custom(_) => event
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| format!("{agent} notification")),
    }
}

fn default_body(event: &Event, message: Option<&str>) -> String {
    let agent = &event.agent_name;
    let fallback = match &event.event_type {
        EventType::PermissionRequest => {
            format!("{agent} is waiting for permission to continue.")
        }
        EventType::UserQuestion => format!("{agent} has a question for you."),
        EventType::TaskCompleted => format!("{agent} has completed the current task."),
        EventType::TaskFailed => {
            "The agent stopped because of an error and may need your attention.".to_owned()
        }
        EventType::AgentIdle => format!("{agent} is idle and waiting for your input."),
        EventType::AgentStarted => format!("A new {agent} session started."),
        EventType::AgentStopped => format!("The {agent} session ended."),
        EventType::Activity | EventType::Notification | EventType::Custom(_) => {
            format!("{agent} needs your attention.")
        }
    };
    match (&event.event_type, message) {
        (_, None) => fallback,
        (EventType::TaskCompleted, Some(m)) => format!("{fallback}\n\n{m}"),
        (EventType::TaskFailed, Some(m)) => format!("{fallback}\n\n{m}"),
        (_, Some(m)) => m.to_owned(),
    }
}

/// Truncates to at most `max` characters, appending an ellipsis when shortened.
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out = out.trim_end().to_owned();
    out.push('…');
    out
}

/// Renders a notification for `event` with the effective `severity`.
pub fn render(event: &Event, severity: Severity, config: &Config) -> RenderedNotification {
    let prefs = &config.notifications;
    let message = event
        .message
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty() && prefs.include_message)
        .map(|m| truncate(&redact(m), prefs.max_message_chars));
    let emoji = emoji(&event.event_type, severity);
    let project = event.project_name.clone();

    let template = config
        .templates
        .get(event.event_type.as_str())
        .or_else(|| config.templates.get("default"));
    let vars = TemplateVars {
        event,
        severity,
        emoji,
        message: message.as_deref(),
    };
    let title = match template.and_then(|t: &TemplateConfig| t.title.as_deref()) {
        Some(t) => vars.apply(t),
        None => format!("{emoji} {}", default_title(event)),
    };
    let body = match template.and_then(|t| t.body.as_deref()) {
        Some(t) => vars.apply(t),
        None => default_body(event, message.as_deref()),
    };

    let mut fields = Vec::new();
    if let Some(p) = &project {
        fields.push(Field {
            name: "Project".into(),
            value: p.clone(),
        });
    }
    fields.push(Field {
        name: "Agent".into(),
        value: event.agent_name.clone(),
    });
    fields.push(Field {
        name: "Event".into(),
        value: event.event_type.label().to_owned(),
    });
    if prefs.include_project_path {
        if let Some(path) = event.project_path.as_deref().or(event.cwd.as_deref()) {
            fields.push(Field {
                name: "Path".into(),
                value: path.to_owned(),
            });
        }
    }
    let footer = match event.session_short() {
        Some(s) => format!("AgentPing · session {s}"),
        None => "AgentPing".to_owned(),
    };

    RenderedNotification {
        event_id: event.id.to_string(),
        event_type: event.event_type.clone(),
        severity,
        title: truncate(title.trim(), 240),
        body,
        fields,
        footer,
        occurred_at: event.occurred_at,
        project,
    }
}

struct TemplateVars<'a> {
    event: &'a Event,
    severity: Severity,
    emoji: &'a str,
    message: Option<&'a str>,
}

impl TemplateVars<'_> {
    fn value(&self, name: &str) -> String {
        match name {
            "project" => self
                .event
                .project_name
                .clone()
                .unwrap_or_else(|| "unknown project".into()),
            "agent" => self.event.agent_name.clone(),
            "event" => self.event.event_type.label().to_owned(),
            "message" => self.message.unwrap_or("").to_owned(),
            "title" => self.event.title.clone().unwrap_or_default(),
            "session" => self.event.session_short().unwrap_or("").to_owned(),
            "severity" => self.severity.to_string(),
            "time" => self
                .event
                .occurred_at
                .with_timezone(&Local)
                .format("%H:%M")
                .to_string(),
            "emoji" => self.emoji.to_owned(),
            other => format!("{{{other}}}"),
        }
    }

    fn apply(&self, template: &str) -> String {
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(start) = rest.find('{') {
            out.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            match after.find('}') {
                Some(end)
                    if after[..end]
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_') =>
                {
                    out.push_str(&self.value(&after[..end]));
                    rest = &after[end + 1..];
                }
                _ => {
                    out.push('{');
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }
}

/// A synthetic event used by `agentping test`.
pub fn test_event() -> Event {
    let mut e = Event::new(
        EventType::Notification,
        agentping_core::AgentKind::Other("agentping".into()),
        Utc::now(),
    );
    e.agent_name = "AgentPing".into();
    e.title = Some("AgentPing test notification".into());
    e.message =
        Some("If you can read this on your phone, AgentPing is configured correctly. 🎉".into());
    e.project_name = Some("agentping".into());
    e.source.integration = "agentping-cli".into();
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentping_core::AgentKind;

    fn perm() -> Event {
        let mut e = Event::new(EventType::PermissionRequest, AgentKind::Claude, Utc::now());
        e.project_name = Some("backend-api".into());
        e.project_path = Some("/home/me/backend-api".into());
        e.session_id = Some("0123456789abcdef".into());
        e.message = Some("Claude needs your permission to use Bash".into());
        e
    }

    #[test]
    fn permission_request_matches_spec_shape() {
        let n = render(&perm(), Severity::High, &Config::default());
        assert_eq!(n.title, "🔐 Claude Code needs permission");
        assert_eq!(n.body, "Claude needs your permission to use Bash");
        assert_eq!(n.fields[0].value, "backend-api");
        assert_eq!(n.fields[1].value, "Claude Code");
        assert_eq!(n.fields[2].value, "Permission request");
        assert_eq!(n.footer, "AgentPing · session 01234567");
        assert!(
            n.fields.iter().all(|f| f.name != "Path"),
            "paths hidden by default"
        );
    }

    #[test]
    fn message_can_be_disabled_and_paths_enabled() {
        let mut c = Config::default();
        c.notifications.include_message = false;
        c.notifications.include_project_path = true;
        let n = render(&perm(), Severity::High, &c);
        assert_eq!(n.body, "Claude Code is waiting for permission to continue.");
        assert!(n.fields.iter().any(|f| f.name == "Path"));
    }

    #[test]
    fn message_is_redacted_and_truncated() {
        let mut e = perm();
        e.message = Some(format!(
            "use key sk-ant-abcdefghijklmnopqrstuvwxyz {}",
            "x".repeat(2000)
        ));
        let n = render(&e, Severity::High, &Config::default());
        assert!(!n.body.contains("abcdefghijklmnop"));
        assert!(n.body.chars().count() <= 600);
        assert!(n.body.ends_with('…'));
    }

    #[test]
    fn missing_fields_render_gracefully() {
        let e = Event::new(EventType::TaskCompleted, AgentKind::Claude, Utc::now());
        let n = render(&e, Severity::Normal, &Config::default());
        assert_eq!(n.title, "✅ Claude Code completed its task");
        assert_eq!(n.body, "Claude Code has completed the current task.");
        assert_eq!(n.footer, "AgentPing");
        assert!(n.fields.iter().all(|f| f.name != "Project"));
    }

    #[test]
    fn templates_override_defaults() {
        let mut c = Config::default();
        c.templates.insert(
            "permission_request".into(),
            TemplateConfig {
                title: Some("{emoji} [{project}] {agent} wants {event}".into()),
                body: Some("{message} (severity {severity}) {unknown} {not valid}".into()),
            },
        );
        let n = render(&perm(), Severity::Urgent, &c);
        assert_eq!(
            n.title,
            "🔐 [backend-api] Claude Code wants Permission request"
        );
        assert_eq!(
            n.body,
            "Claude needs your permission to use Bash (severity urgent) {unknown} {not valid}"
        );
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        assert_eq!(truncate("héllo wörld", 6), "héllo…");
        assert_eq!(truncate("short", 10), "short");
    }
}
