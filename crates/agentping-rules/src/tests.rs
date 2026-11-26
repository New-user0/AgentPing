use agentping_core::config::RuleConfig;
use agentping_core::{AgentKind, Config, Event, EventType, Severity};
use chrono::{NaiveTime, Utc};

use super::*;

fn config() -> Config {
    let mut c = Config::default();
    c.providers.discord.enabled = true;
    c.providers.telegram.enabled = true;
    c.providers.telegram.chat_id = Some("1".into());
    c
}

fn event(t: EventType, project: &str) -> Event {
    let mut e = Event::new(t, AgentKind::Claude, Utc::now());
    e.session_id = Some("s1".into());
    e.project_name = Some(project.into());
    e.project_path = Some(format!("D:\\work\\{project}"));
    e
}

fn noon() -> RuleContext {
    RuleContext {
        now_local: NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
        recently_notified: false,
    }
}

fn eval(c: &Config, e: &Event, ctx: RuleContext) -> Decision {
    RulesEngine::new(c).unwrap().evaluate(e, &ctx)
}

#[test]
fn default_preferences_notify_all_enabled_providers() {
    let d = eval(
        &config(),
        &event(EventType::PermissionRequest, "api"),
        noon(),
    );
    assert!(d.notify);
    assert_eq!(d.providers, vec!["discord", "telegram"]);
    assert_eq!(d.reason, "default preferences");
}

#[test]
fn activity_and_disabled_switches_mute() {
    let c = config();
    assert!(!eval(&c, &event(EventType::Activity, "api"), noon()).notify);

    let mut off = config();
    off.general.notifications_enabled = false;
    assert!(!eval(&off, &event(EventType::PermissionRequest, "api"), noon()).notify);

    let mut no_claude = config();
    no_claude.agents.claude.enabled = false;
    assert!(
        !eval(
            &no_claude,
            &event(EventType::PermissionRequest, "api"),
            noon()
        )
        .notify
    );
}

#[test]
fn type_toggles_and_project_filters_apply_without_rules() {
    let mut c = config();
    c.notifications.task_completed = false;
    let d = eval(&c, &event(EventType::TaskCompleted, "api"), noon());
    assert!(!d.notify);
    assert!(d.reason.contains("task_completed"));

    let mut c = config();
    c.notifications.filters.exclude_projects = vec!["Sandbox".into()];
    assert!(!eval(&c, &event(EventType::PermissionRequest, "sandbox"), noon()).notify);
    assert!(eval(&c, &event(EventType::PermissionRequest, "api"), noon()).notify);

    let mut c = config();
    c.notifications.filters.include_projects = vec!["api".into()];
    assert!(eval(&c, &event(EventType::PermissionRequest, "api"), noon()).notify);
    assert!(!eval(&c, &event(EventType::PermissionRequest, "web"), noon()).notify);
}

#[test]
fn first_matching_rule_wins_and_can_override_toggles() {
    let mut c = config();
    c.notifications.task_completed = false;
    c.rules = vec![
        RuleConfig {
            name: "mute sandbox".into(),
            projects: vec!["sandbox".into()],
            notify: false,
            ..Default::default()
        },
        RuleConfig {
            name: "completions for api".into(),
            projects: vec!["api".into()],
            event_types: vec!["task_completed".into()],
            providers: vec!["telegram".into()],
            ..Default::default()
        },
    ];
    let d = eval(&c, &event(EventType::TaskCompleted, "api"), noon());
    assert!(d.notify, "{d:?}");
    assert_eq!(d.providers, vec!["telegram"]);
    assert_eq!(d.reason, "rule 'completions for api'");

    let d = eval(&c, &event(EventType::PermissionRequest, "sandbox"), noon());
    assert!(!d.notify);
    assert!(d.reason.contains("mute sandbox"));
}

#[test]
fn rules_match_on_path_globs_agents_and_severity() {
    let mut c = config();
    c.rules = vec![RuleConfig {
        name: "work only discord".into(),
        project_paths: vec!["D:/work/**".into()],
        agents: vec!["claude".into()],
        min_severity: Some(Severity::High),
        providers: vec!["discord".into()],
        ..Default::default()
    }];
    let d = eval(&c, &event(EventType::PermissionRequest, "api"), noon());
    assert_eq!(d.providers, vec!["discord"]);
    // Below min_severity: the rule does not match, defaults apply.
    let d = eval(&c, &event(EventType::TaskCompleted, "api"), noon());
    assert_eq!(d.providers, vec!["discord", "telegram"]);
    // Different agent: no match.
    let mut other = event(EventType::PermissionRequest, "api");
    other.agent = AgentKind::Other("codex".into());
    assert_eq!(
        eval(&c, &other, noon()).providers,
        vec!["discord", "telegram"]
    );
}

#[test]
fn rule_targeting_disabled_provider_mutes_with_explanation() {
    let mut c = config();
    c.providers.telegram.enabled = false;
    c.rules = vec![RuleConfig {
        name: "tg".into(),
        providers: vec!["telegram".into()],
        ..Default::default()
    }];
    let d = eval(&c, &event(EventType::PermissionRequest, "api"), noon());
    assert!(!d.notify);
    assert!(d.reason.contains("not enabled"));
}

#[test]
fn no_providers_means_no_notification() {
    let c = Config::default();
    let d = eval(&c, &event(EventType::PermissionRequest, "api"), noon());
    assert!(!d.notify);
    assert_eq!(d.reason, "no notification providers are enabled");
}

#[test]
fn quiet_hours_allow_urgent_and_wrap_midnight() {
    let mut c = config();
    c.quiet_hours.enabled = true;
    c.quiet_hours.start = "22:00".into();
    c.quiet_hours.end = "07:00".into();
    c.quiet_hours.min_severity = Severity::Urgent;
    let night = RuleContext {
        now_local: NaiveTime::from_hms_opt(23, 30, 0).unwrap(),
        recently_notified: false,
    };
    let early = RuleContext {
        now_local: NaiveTime::from_hms_opt(6, 59, 0).unwrap(),
        recently_notified: false,
    };
    let perm = event(EventType::PermissionRequest, "api");
    assert!(!eval(&c, &perm, night).notify);
    assert!(!eval(&c, &perm, early).notify);
    assert!(eval(&c, &perm, noon()).notify);

    c.rules = vec![RuleConfig {
        name: "urgent permissions".into(),
        event_types: vec!["permission_request".into()],
        severity: Some(Severity::Urgent),
        ..Default::default()
    }];
    let d = eval(&c, &perm, night);
    assert!(d.notify);
    assert_eq!(d.severity, Severity::Urgent);
}

#[test]
fn repeat_suppression_only_for_low_value_events() {
    let c = config();
    let repeat = RuleContext {
        recently_notified: true,
        ..noon()
    };
    assert!(!eval(&c, &event(EventType::AgentIdle, "api"), repeat).notify);
    assert!(!eval(&c, &event(EventType::TaskCompleted, "api"), repeat).notify);
    assert!(eval(&c, &event(EventType::PermissionRequest, "api"), repeat).notify);
    assert!(eval(&c, &event(EventType::UserQuestion, "api"), repeat).notify);

    let mut no_window = config();
    no_window.notifications.repeat_window_secs = 0;
    assert!(eval(&no_window, &event(EventType::AgentIdle, "api"), repeat).notify);
}

#[test]
fn window_math() {
    let t = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
    assert!(in_window(t(10, 0), t(9, 0), t(17, 0)));
    assert!(!in_window(t(17, 0), t(9, 0), t(17, 0)));
    assert!(in_window(t(0, 0), t(22, 0), t(7, 0)));
    assert!(!in_window(t(12, 0), t(22, 0), t(7, 0)));
    assert!(!in_window(t(12, 0), t(12, 0), t(12, 0)));
}

#[test]
fn decision_summary_is_readable() {
    let d = eval(
        &config(),
        &event(EventType::PermissionRequest, "api"),
        noon(),
    );
    assert_eq!(
        d.summary(),
        "notify via discord, telegram (default preferences)"
    );
}

#[test]
fn remote_routing_follows_project_policy_but_not_quiet_hours_or_providers() {
    let mut c = Config::default(); // no providers enabled at all
    c.notifications.filters.exclude_projects = vec!["sandbox".into()];
    c.quiet_hours.enabled = true;
    c.quiet_hours.start = "00:00".into();
    c.quiet_hours.end = "23:59".into();
    c.rules = vec![RuleConfig {
        name: "mute client work".into(),
        projects: vec!["client".into()],
        notify: false,
        ..Default::default()
    }];
    let engine = RulesEngine::new(&c).unwrap();
    assert!(engine
        .remote_allowed(&event(EventType::PermissionRequest, "api"))
        .is_ok());
    let err = engine
        .remote_allowed(&event(EventType::PermissionRequest, "sandbox"))
        .unwrap_err();
    assert!(err.contains("sandbox"), "{err}");
    assert!(engine
        .remote_allowed(&event(EventType::UserQuestion, "client"))
        .unwrap_err()
        .contains("mute client work"));
    c.notifications.permission_requests = false;
    let engine = RulesEngine::new(&c).unwrap();
    assert!(engine
        .remote_allowed(&event(EventType::PermissionRequest, "api"))
        .is_err());
}
