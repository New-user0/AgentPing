//! Optional OS background-service registration for the daemon.
//!
//! * Linux: systemd **user** unit (`~/.config/systemd/user/agentping.service`)
//! * macOS: launchd agent (`~/Library/LaunchAgents/dev.agentping.daemon.plist`)
//! * Windows: a per-user `HKCU\...\CurrentVersion\Run` entry (no administrator rights needed;
//!   runs as you, so the Credential Manager is available; a Windows *service* would run as
//!   SYSTEM and could not read it)

use std::path::{Path, PathBuf};
use std::process::Command;

/// Registry key holding per-user login items on Windows.
pub const WINDOWS_RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
/// Value name of AgentPing's login item.
pub const WINDOWS_RUN_VALUE: &str = "AgentPing";
/// systemd unit name.
pub const SYSTEMD_UNIT: &str = "agentping.service";
/// launchd label.
pub const LAUNCHD_LABEL: &str = "dev.agentping.daemon";

/// Renders the systemd user unit.
pub fn systemd_unit(exe: &Path) -> String {
    format!(
        "[Unit]\nDescription=AgentPing daemon (AI agent notifications)\nAfter=network-online.target\n\n\
         [Service]\nType=simple\nExecStart=\"{}\" daemon run\nRestart=on-failure\nRestartSec=5\n\n\
         [Install]\nWantedBy=default.target\n",
        exe.display()
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Renders the launchd agent plist.
pub fn launchd_plist(exe: &Path, log_dir: &Path) -> String {
    let exe = xml_escape(&exe.display().to_string());
    let log = xml_escape(&log_dir.join("daemon-launchd.log").display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>daemon</string>
    <string>run</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#
    )
}

/// Command line registered as the Windows login item.
pub fn windows_run_command(exe: &Path) -> String {
    format!("\"{}\" daemon start", exe.display())
}

/// Where the service definition lives on this platform, if file-based.
pub fn definition_path() -> Option<PathBuf> {
    let home = agentping_core::paths::home_dir()?;
    if cfg!(target_os = "linux") {
        Some(home.join(".config/systemd/user").join(SYSTEMD_UNIT))
    } else if cfg!(target_os = "macos") {
        Some(
            home.join("Library/LaunchAgents")
                .join(format!("{LAUNCHD_LABEL}.plist")),
        )
    } else {
        None
    }
}

fn run(program: &str, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| anyhow::anyhow!("could not run {program}: {e}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        Ok(text)
    } else {
        anyhow::bail!("{program} failed: {}", text.trim())
    }
}

/// Registers the daemon to start at login and starts it. Returns a description of what was done.
pub fn install(exe: &Path, log_dir: &Path) -> anyhow::Result<String> {
    if cfg!(windows) {
        let command = windows_run_command(exe);
        run(
            "reg",
            &[
                "add",
                WINDOWS_RUN_KEY,
                "/v",
                WINDOWS_RUN_VALUE,
                "/t",
                "REG_SZ",
                "/d",
                &command,
                "/f",
            ],
        )?;
        Ok("Registered AgentPing to start when you log in to Windows".to_owned())
    } else if cfg!(target_os = "linux") {
        let path = definition_path().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
        agentping_core::fsutil::atomic_write(&path, systemd_unit(exe).as_bytes(), false)?;
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", "--now", SYSTEMD_UNIT])?;
        Ok(format!(
            "Installed and started systemd user unit {}",
            path.display()
        ))
    } else if cfg!(target_os = "macos") {
        let path = definition_path().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
        agentping_core::fsutil::atomic_write(&path, launchd_plist(exe, log_dir).as_bytes(), false)?;
        let p = path.to_string_lossy().into_owned();
        let _ = run("launchctl", &["unload", &p]);
        run("launchctl", &["load", "-w", &p])?;
        Ok(format!(
            "Installed and loaded launchd agent {}",
            path.display()
        ))
    } else {
        anyhow::bail!("background service installation is not supported on this platform; run `agentping daemon start` from your login scripts instead")
    }
}

/// Removes the service registration.
pub fn uninstall() -> anyhow::Result<String> {
    if cfg!(windows) {
        run(
            "reg",
            &["delete", WINDOWS_RUN_KEY, "/v", WINDOWS_RUN_VALUE, "/f"],
        )?;
        Ok("Removed AgentPing from Windows login items".to_owned())
    } else if cfg!(target_os = "linux") {
        let _ = run("systemctl", &["--user", "disable", "--now", SYSTEMD_UNIT]);
        if let Some(path) = definition_path() {
            let _ = std::fs::remove_file(&path);
        }
        let _ = run("systemctl", &["--user", "daemon-reload"]);
        Ok("Removed systemd user unit".into())
    } else if cfg!(target_os = "macos") {
        if let Some(path) = definition_path() {
            let _ = run("launchctl", &["unload", "-w", &path.to_string_lossy()]);
            let _ = std::fs::remove_file(&path);
        }
        Ok("Removed launchd agent".into())
    } else {
        anyhow::bail!("background service installation is not supported on this platform")
    }
}

/// Whether the service is registered.
pub fn is_installed() -> bool {
    if cfg!(windows) {
        Command::new("reg")
            .args(["query", WINDOWS_RUN_KEY, "/v", WINDOWS_RUN_VALUE])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    } else {
        definition_path().is_some_and(|p| p.is_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_unit_runs_daemon_in_foreground() {
        let unit = systemd_unit(Path::new("/home/u/.cargo/bin/agentping"));
        assert!(unit.contains("ExecStart=\"/home/u/.cargo/bin/agentping\" daemon run"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("Restart=on-failure"));
    }

    #[test]
    fn launchd_plist_is_escaped() {
        let plist = launchd_plist(Path::new("/Users/a&b/agentping"), Path::new("/tmp/logs"));
        assert!(plist.contains("<string>/Users/a&amp;b/agentping</string>"));
        assert!(plist.contains(LAUNCHD_LABEL));
        assert!(plist.contains("<string>run</string>"));
    }

    #[test]
    fn windows_run_command_quotes_path() {
        assert_eq!(
            windows_run_command(Path::new(r"C:\Program Files\agentping.exe")),
            r#""C:\Program Files\agentping.exe" daemon start"#
        );
    }
}
