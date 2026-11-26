#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

/// A shape-valid but fake webhook (never contacted: delivery spawning is disabled in tests).
const FAKE_WEBHOOK: &str =
    "https://discord.com/api/webhooks/123456789012345678/FAKE-token_abcdefghijklmnopqrstuvwxyz0123";

struct Env {
    home: tempfile::TempDir,
    cwd: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            cwd: tempfile::tempdir().unwrap(),
        }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("agentping").unwrap();
        c.env("AGENTPING_HOME", self.home.path())
            .env("AGENTPING_DISABLE_KEYRING", "1")
            // An unused port so no real daemon on this machine is contacted.
            .env("AGENTPING_DAEMON_PORT", "1")
            .env("CLAUDE_CONFIG_DIR", self.home.path().join("claude"))
            .env("NO_COLOR", "1")
            .env_remove("AGENTPING_DISCORD_WEBHOOK_URL")
            .env_remove("AGENTPING_TELEGRAM_BOT_TOKEN")
            .env_remove("CLAUDE_PROJECT_DIR")
            .current_dir(self.cwd.path());
        c
    }

    fn config_path(&self) -> PathBuf {
        self.home.path().join("config").join("config.toml")
    }

    fn enable_fake_discord(&self) {
        self.cmd()
            .args(["init", "--non-interactive", "--no-claude"])
            .assert()
            .success();
        for (k, v) in [
            ("providers.discord.enabled", "true"),
            ("daemon.spawn_delivery_when_offline", "false"),
        ] {
            self.cmd().args(["config", "set", k, v]).assert().success();
        }
    }

    fn hook(&self, fixture: &str, with_webhook: bool) {
        let mut c = self.cmd();
        if with_webhook {
            c.env("AGENTPING_DISCORD_WEBHOOK_URL", FAKE_WEBHOOK);
        }
        c.args(["hook", "claude"])
            .write_stdin(std::fs::read(fixture_path(fixture)).unwrap())
            .assert()
            .success()
            .stdout("")
            .stderr("");
    }

    fn events_json(&self) -> Vec<Value> {
        let out = self.cmd().args(["--json", "events"]).output().unwrap();
        assert!(out.status.success());
        serde_json::from_slice::<Vec<Value>>(&out.stdout).unwrap()
    }
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/claude")
        .join(name)
}

#[test]
fn help_and_version() {
    let env = Env::new();
    env.cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Stay connected to your AI coding agents",
        ))
        .stdout(predicate::str::contains("doctor"));
    env.cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn hook_never_prints_or_fails_on_bad_input() {
    let env = Env::new();
    for fixture in [
        "invalid_truncated.json",
        "not_an_object.json",
        "missing_event_name.json",
        "wrong_field_types.json",
        "subagent_stop.json",
    ] {
        env.hook(fixture, false);
    }
    env.cmd()
        .args(["hook", "claude"])
        .write_stdin("")
        .assert()
        .success()
        .stdout("")
        .stderr("");
    env.cmd()
        .args(["hook", "some-unknown-agent"])
        .write_stdin("{}")
        .assert()
        .success()
        .stdout("")
        .stderr("");
}

#[test]
fn hook_event_is_recorded_without_daemon() {
    let env = Env::new();
    env.hook("notification_permission_prompt.json", false);
    let events = env.events_json();
    assert_eq!(events.len(), 1);
    let e = &events[0]["event"];
    assert_eq!(e["event_type"], "permission_request");
    assert_eq!(e["project_name"], "backend-api");
    assert_eq!(
        events[0]["decision"],
        "muted: no notification providers are enabled"
    );
    // The same hook firing twice (e.g. installed in two settings files) is deduplicated.
    env.hook("notification_permission_prompt.json", false);
    assert_eq!(env.events_json().len(), 1);
}

#[test]
fn notifications_are_queued_and_questions_cancelled_when_answered() {
    let env = Env::new();
    env.enable_fake_discord();
    env.hook("notification_permission_prompt.json", true);
    env.hook("pre_tool_use_ask_user_question.json", true);
    env.hook("post_tool_use_ask_user_question.json", true);

    let events = env.events_json();
    let by_type = |t: &str| {
        events
            .iter()
            .find(|e| e["event"]["event_type"] == t)
            .unwrap_or_else(|| panic!("no {t} event"))
            .clone()
    };
    let perm = by_type("permission_request");
    assert_eq!(perm["deliveries"][0]["provider"], "discord");
    assert_eq!(perm["deliveries"][0]["status"], "pending");
    let question = by_type("user_question");
    assert_eq!(question["deliveries"][0]["status"], "cancelled");

    env.cmd()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Pending: 1"));
}

#[test]
fn prompt_text_never_reaches_storage() {
    let env = Env::new();
    env.hook("user_prompt_submit.json", false);
    let db = std::fs::read(env.home.path().join("data").join("agentping.db")).unwrap();
    let text = String::from_utf8_lossy(&db);
    assert!(!text.contains("hunter2"));
}

#[test]
fn claude_hooks_install_preserves_settings_and_uninstalls_cleanly() {
    let env = Env::new();
    let settings_dir = env.cwd.path().join(".claude");
    std::fs::create_dir_all(&settings_dir).unwrap();
    let path = settings_dir.join("settings.local.json");
    let original = serde_json::json!({
        "permissions": { "allow": ["Bash(npm test)"] },
        "hooks": { "Stop": [ { "hooks": [ { "type": "command", "command": "say done" } ] } ] }
    });
    std::fs::write(&path, serde_json::to_string_pretty(&original).unwrap()).unwrap();

    env.cmd()
        .args(["setup", "claude", "--scope", "local", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Dry run"));
    let untouched: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(untouched, original);

    env.cmd()
        .args(["setup", "claude", "--scope", "local"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Installed 8 Claude Code hooks"));
    let installed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(installed["permissions"], original["permissions"]);
    assert_eq!(
        installed["hooks"]["Stop"][0]["hooks"][0]["command"],
        "say done"
    );
    assert!(installed["hooks"]["Notification"].is_array());
    let backups = std::fs::read_dir(&settings_dir)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("agentping-backup")
        })
        .count();
    assert_eq!(backups, 1);

    env.cmd()
        .args(["setup", "claude", "--scope", "local", "--remove"])
        .assert()
        .success();
    let removed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(removed, original);
}

#[test]
fn config_set_validates_and_rejects_bad_values() {
    let env = Env::new();
    env.cmd()
        .args(["config", "set", "notifications.include_message", "false"])
        .assert()
        .success();
    let text = std::fs::read_to_string(env.config_path()).unwrap();
    assert!(text.contains("include_message = false"));
    assert!(
        text.contains("# AgentPing configuration"),
        "comments preserved"
    );
    env.cmd().args(["config", "validate"]).assert().success();

    env.cmd()
        .args(["config", "set", "daemon.host", "0.0.0.0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("loopback"));
    env.cmd()
        .args(["config", "set", "notifications.typo_key", "true"])
        .assert()
        .failure();
    std::fs::write(env.config_path(), "[general]\nlog_level = 5\n").unwrap();
    env.cmd().args(["config", "validate"]).assert().failure();
}

#[test]
fn discord_setup_rejects_invalid_urls_without_echoing_them() {
    let env = Env::new();
    env.cmd()
        .args(["setup", "discord", "--stdin", "--no-test"])
        .write_stdin("https://evil.example/api/webhooks/1/SUPERSECRETTOKENVALUE123\n")
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid Discord webhook URL"))
        .stderr(predicate::str::contains("SUPERSECRETTOKENVALUE123").not());
}

#[test]
fn discord_setup_stores_secret_outside_config() {
    let env = Env::new();
    env.cmd()
        .args([
            "setup",
            "discord",
            "--stdin",
            "--no-test",
            "--insecure-store",
        ])
        .write_stdin(format!("{FAKE_WEBHOOK}\n"))
        .assert()
        .success();
    let config = std::fs::read_to_string(env.config_path()).unwrap();
    assert!(config.contains("[providers.discord]"));
    assert!(
        !config.contains("FAKE-token"),
        "secret leaked into config.toml"
    );
    let out = env.cmd().args(["--json", "providers"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("FAKE-token"), "secret printed by providers");
    let rows: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(rows[0]["enabled"], true);
    assert_eq!(rows[0]["ready"], true);

    let doctor = env.cmd().args(["doctor", "--offline"]).output().unwrap();
    let text = String::from_utf8_lossy(&doctor.stdout);
    assert!(!text.contains("FAKE-token"), "secret printed by doctor");
    assert!(text.contains("plaintext"));

    env.cmd()
        .args(["setup", "discord", "--disable"])
        .assert()
        .success();
    assert!(!env.home.path().join("config").join("secrets.toml").exists());
}

#[test]
fn events_filters_show_and_clear() {
    let env = Env::new();
    env.hook("notification_permission_prompt.json", false);
    env.hook("stop.json", false);
    let out = env
        .cmd()
        .args(["--json", "events", "--type", "task_completed"])
        .output()
        .unwrap();
    let rows: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(rows.len(), 1);
    let id = rows[0]["event"]["id"].as_str().unwrap().to_owned();
    env.cmd()
        .args(["events", "show", &id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task completed"));
    env.cmd()
        .args(["events", "clear"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--yes"));
    env.cmd()
        .args(["events", "clear", "--yes"])
        .assert()
        .success();
    assert!(env.events_json().is_empty());
}

#[test]
fn rules_test_explains_decisions() {
    let env = Env::new();
    env.enable_fake_discord();
    let out = env
        .cmd()
        .env("AGENTPING_DISCORD_WEBHOOK_URL", FAKE_WEBHOOK)
        .args([
            "--json",
            "rules",
            "test",
            "--type",
            "permission_request",
            "--project",
            "api",
        ])
        .output()
        .unwrap();
    let d: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(d["notify"], true);
    assert_eq!(d["providers"][0], "discord");
    env.cmd()
        .args(["rules", "test", "--type", "bogus"])
        .assert()
        .failure();
}

#[test]
fn whatsapp_is_honestly_unavailable() {
    let env = Env::new();
    env.cmd()
        .args(["setup", "whatsapp"])
        .assert()
        .success()
        .stdout(predicate::str::contains("not implemented"));
    env.cmd()
        .args(["config", "set", "providers.whatsapp.enabled", "true"])
        .assert()
        .failure();
}
