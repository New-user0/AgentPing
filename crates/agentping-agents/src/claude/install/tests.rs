use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use agentping_core::config::RemoteConfig;

use super::*;

fn hooks() -> Vec<ManagedHook> {
    managed_hooks(&RemoteConfig::default())
}

fn hook(timeout: u32) -> ManagedHook {
    ManagedHook {
        event: "Stop",
        matcher: None,
        timeout,
        wait_reply: false,
    }
}

fn exe() -> PathBuf {
    PathBuf::from(if cfg!(windows) {
        r"C:\Users\Dev User\.cargo\bin\agentping.exe"
    } else {
        "/home/dev/.cargo/bin/agentping"
    })
}

fn existing_settings() -> Value {
    json!({
        "model": "opus",
        "permissions": { "allow": ["Bash(npm test)"] },
        "hooks": {
            "Notification": [
                { "hooks": [ { "type": "command", "command": "notify-send 'Claude'" } ] }
            ],
            "PreToolUse": [
                { "matcher": "Bash", "hooks": [ { "type": "command", "command": "./audit.sh" } ] }
            ]
        }
    })
}

fn write(dir: &Path, v: &Value) -> PathBuf {
    let path = dir.join("settings.json");
    std::fs::write(&path, serde_json::to_string_pretty(v).unwrap()).unwrap();
    path
}

fn managed_count(v: &Value) -> usize {
    v["hooks"]
        .as_object()
        .into_iter()
        .flat_map(|h| h.values())
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|g| g["hooks"].as_array())
        .flatten()
        .filter(|h| is_managed_handler(h))
        .count()
}

#[test]
fn install_preserves_existing_settings_and_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), &existing_settings());
    let change = plan_install(&path, &exe(), HookShell::Posix, &hooks()).unwrap();
    assert!(change.changed);
    let after = &change.after;
    assert_eq!(after["model"], "opus");
    assert_eq!(after["permissions"]["allow"][0], "Bash(npm test)");
    // The user's own Notification hook is still first and untouched.
    assert_eq!(
        after["hooks"]["Notification"][0]["hooks"][0]["command"],
        "notify-send 'Claude'"
    );
    // Their Bash matcher group is untouched; ours gets its own AskUserQuestion group.
    let pre = after["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre[0]["matcher"], "Bash");
    assert_eq!(pre[0]["hooks"].as_array().unwrap().len(), 1);
    assert!(pre.iter().any(|g| g["matcher"] == "AskUserQuestion"));
    assert_eq!(managed_count(after), hooks().len());
}

#[test]
fn install_is_idempotent_and_updates_stale_paths() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), &existing_settings());
    plan_install(
        &path,
        Path::new("/old/agentping"),
        HookShell::Posix,
        &hooks(),
    )
    .unwrap()
    .apply()
    .unwrap();
    let second = plan_install(&path, &exe(), HookShell::Posix, &hooks()).unwrap();
    assert!(second.changed, "path change should be applied");
    assert_eq!(managed_count(&second.after), hooks().len());
    second.apply().unwrap();
    let third = plan_install(&path, &exe(), HookShell::Posix, &hooks()).unwrap();
    assert!(!third.changed, "re-installing the same binary is a no-op");
}

#[test]
fn apply_creates_backup_and_valid_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), &existing_settings());
    let original = std::fs::read_to_string(&path).unwrap();
    let backup = plan_install(&path, &exe(), HookShell::Posix, &hooks())
        .unwrap()
        .apply()
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(managed_count(&written), hooks().len());
}

#[test]
fn install_into_missing_file_creates_it_without_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".claude").join("settings.local.json");
    let change = plan_install(&path, &exe(), HookShell::Posix, &hooks()).unwrap();
    assert!(!change.existed);
    assert_eq!(change.apply().unwrap(), None);
    assert!(path.is_file());
}

#[test]
fn uninstall_removes_only_managed_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), &existing_settings());
    plan_install(&path, &exe(), HookShell::Posix, &hooks())
        .unwrap()
        .apply()
        .unwrap();
    let change = plan_uninstall(&path).unwrap();
    assert!(change.changed);
    assert_eq!(change.after, existing_settings());
    change.apply().unwrap();
    assert!(!plan_uninstall(&path).unwrap().changed);
}

#[test]
fn uninstall_drops_hooks_key_only_if_we_emptied_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), &json!({ "model": "opus" }));
    plan_install(&path, &exe(), HookShell::Posix, &hooks())
        .unwrap()
        .apply()
        .unwrap();
    let after = plan_uninstall(&path).unwrap().after;
    assert_eq!(after, json!({ "model": "opus" }));

    // An empty hooks object that the user created stays.
    let path = write(dir.path(), &json!({ "hooks": {} }));
    assert!(!plan_uninstall(&path).unwrap().changed);
}

#[test]
fn invalid_or_unexpected_settings_are_refused_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(&path, "{ not json").unwrap();
    assert!(matches!(
        plan_install(&path, &exe(), HookShell::Posix, &hooks()),
        Err(InstallError::InvalidJson { .. })
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");

    let path = write(dir.path(), &json!({ "hooks": [] }));
    assert!(matches!(
        plan_install(&path, &exe(), HookShell::Posix, &hooks()),
        Err(InstallError::UnexpectedShape(_))
    ));
}

#[test]
fn handler_shapes_per_shell() {
    let posix = handler(
        Path::new("/usr/local/bin/agentping"),
        HookShell::Posix,
        &hook(10),
    );
    assert_eq!(posix["command"], "/usr/local/bin/agentping hook claude");
    assert!(posix.get("shell").is_none());

    let spaced = handler(
        Path::new("/Users/Dev User/bin/agentping"),
        HookShell::Posix,
        &hook(10),
    );
    assert_eq!(
        spaced["command"],
        "'/Users/Dev User/bin/agentping' hook claude"
    );

    let bash = handler(
        Path::new(r"C:\Users\Dev User\agentping.exe"),
        HookShell::GitBash,
        &hook(10),
    );
    assert_eq!(
        bash["command"],
        "'C:/Users/Dev User/agentping.exe' hook claude"
    );
    assert_eq!(bash["shell"], "bash");

    let ps = handler(
        Path::new(r"C:\Users\O'Neil\agentping.exe"),
        HookShell::PowerShell,
        &hook(5),
    );
    assert_eq!(
        ps["command"],
        r"& 'C:\Users\O''Neil\agentping.exe' hook claude"
    );
    assert_eq!(ps["shell"], "powershell");
    assert_eq!(ps["timeout"], 5);

    for h in [posix, spaced, bash, ps] {
        assert!(is_managed_handler(&h));
    }
    assert!(!is_managed_handler(
        &json!({ "command": "agentping-unrelated --flag" })
    ));
}

#[test]
fn inspect_reports_missing_and_broken_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), &existing_settings());
    let status = inspect(&path, &hooks()).unwrap();
    assert!(status.installed.is_empty());
    assert_eq!(status.missing.len(), hooks().len());

    plan_install(
        &path,
        Path::new("/nonexistent/agentping"),
        HookShell::Posix,
        &hooks(),
    )
    .unwrap()
    .apply()
    .unwrap();
    let status = inspect(&path, &hooks()).unwrap();
    assert!(status.missing.is_empty());
    assert_eq!(
        status.broken_commands,
        vec!["/nonexistent/agentping".to_owned()]
    );
    assert!(!status.is_complete());

    let real = std::env::current_exe().unwrap();
    plan_install(&path, &real, HookShell::Posix, &hooks())
        .unwrap()
        .apply()
        .unwrap();
    assert!(inspect(&path, &hooks()).unwrap().is_complete());
}

#[test]
fn remote_hooks_get_long_timeouts_and_a_background_reply_waiter() {
    let remote = RemoteConfig {
        enabled: true,
        decision_timeout_minutes: 10,
        reply_timeout_minutes: 60,
        ..RemoteConfig::default()
    };
    let managed = managed_hooks(&remote);
    let perm = managed
        .iter()
        .find(|h| h.event == "PermissionRequest")
        .unwrap();
    assert_eq!(perm.timeout, 630);
    let waiter = managed.iter().find(|h| h.wait_reply).unwrap();
    assert_eq!((waiter.event, waiter.timeout), ("Stop", 3660));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let real = std::env::current_exe().unwrap();
    plan_install(&path, &real, HookShell::Posix, &managed)
        .unwrap()
        .apply()
        .unwrap();
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let stop = v["hooks"]["Stop"][0]["hooks"].as_array().unwrap();
    assert_eq!(stop.len(), 2);
    let w = stop
        .iter()
        .find(|h| h["command"].as_str().unwrap().ends_with("--wait-reply"))
        .unwrap();
    assert_eq!(w["asyncRewake"], true);
    assert!(inspect(&path, &managed).unwrap().is_complete());
    // Re-installing without remote removes the waiter.
    let plain = plan_install(&path, &real, HookShell::Posix, &hooks()).unwrap();
    assert!(plain.changed);
    assert!(!plain.after.to_string().contains("--wait-reply"));
    assert!(!plan_uninstall(&path)
        .unwrap()
        .after
        .to_string()
        .contains("agentping"));
}
