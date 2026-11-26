//! Spawning detached background processes (daemon, one-shot delivery) from short-lived
//! processes such as hooks, without making the parent's caller wait for the child.

use std::path::Path;
use std::process::{Command, Stdio};

/// Starts `exe args…` fully detached: no console window, no inherited stdio, own process
/// group/session. Returns the child's pid.
pub fn spawn_detached(exe: &Path, args: &[&str]) -> std::io::Result<u32> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    platform::prepare(&mut cmd);
    let child = platform::spawn(&mut cmd)?;
    Ok(child.id())
}

#[cfg(unix)]
mod platform {
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    pub fn prepare(cmd: &mut Command) {
        // New process group so terminal signals aimed at the parent don't reach the child.
        cmd.process_group(0);
    }

    pub fn spawn(cmd: &mut Command) -> std::io::Result<Child> {
        cmd.spawn()
    }
}

#[cfg(windows)]
mod platform {
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command};

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

    pub fn prepare(_cmd: &mut Command) {
        // Windows children inherit every *inheritable* handle of the parent, not just the ones
        // passed as stdio. A hook's stdout/stderr are pipes the agent reads until EOF, so a
        // child holding them would make the agent wait for the child to exit. Mark our standard
        // handles non-inheritable first.
        disinherit_std_handles();
    }

    pub fn spawn(cmd: &mut Command) -> std::io::Result<Child> {
        // Try to leave the caller's job object so the child survives the hook's cleanup; fall
        // back when the job does not allow breakaway.
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB);
        match cmd.spawn() {
            Ok(child) => Ok(child),
            Err(_) => {
                cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
                cmd.spawn()
            }
        }
    }

    #[allow(unsafe_code)]
    fn disinherit_std_handles() {
        use windows_sys::Win32::Foundation::{
            SetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
        };
        use windows_sys::Win32::System::Console::{
            GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        };
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: GetStdHandle has no preconditions and returns either a valid handle,
            // null, or INVALID_HANDLE_VALUE. SetHandleInformation only changes the inherit flag
            // of a handle this process owns; failures are ignored (best effort).
            unsafe {
                let handle = GetStdHandle(which);
                if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                    SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
                }
            }
        }
    }
}
