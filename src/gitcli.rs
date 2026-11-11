//! The system `git` executable. Network operations and everything that rewrites history go
//! through it, so the user's SSH agent, credential helper, hooks and signing setup apply.

use serde::Serialize;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitInfo {
    /// For example `2.51.0.windows.1`.
    pub version: String,
}

/// A `git` command that never opens a console window and never waits for terminal input.
pub fn command() -> Command {
    let mut cmd = Command::new("git");
    cmd.stdin(Stdio::null());
    // Fail instead of asking for a password on a terminal nobody sees.
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    // Stable, parseable messages.
    cmd.env("LC_ALL", "C");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Looks for `git` on PATH. `None` means reading still works but network and history
/// operations do not.
pub fn detect() -> Option<GitInfo> {
    let out = command().arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let version = text.trim().strip_prefix("git version ").unwrap_or(text.trim()).to_string();
    Some(GitInfo { version })
}
