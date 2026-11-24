//! The system `git` executable. Network operations and everything that rewrites history go
//! through it, so the user's SSH agent, credential helper, hooks and signing setup apply.

use serde::Serialize;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitInfo {
    /// For example `2.51.0.windows.1`.
    pub version: String,
}

/// A failed git command, shown to the user word for word.
#[derive(Debug, Clone)]
pub struct GitError {
    /// The command line as the user could type it.
    pub command: String,
    pub code: Option<i32>,
    /// stderr followed by stdout, trimmed.
    pub output: String,
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.output.is_empty() {
            write!(f, "`{}` failed with exit code {}", self.command, self.code.map_or("?".to_string(), |c| c.to_string()))
        } else {
            write!(f, "{}", self.output)
        }
    }
}

impl std::error::Error for GitError {}

/// A `git` command that never opens a console window and never waits for terminal input.
pub fn command() -> Command {
    let mut cmd = Command::new("git");
    cmd.stdin(Stdio::null());
    // Fail instead of asking for a password on a terminal nobody sees.
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    // Stable, parseable messages.
    cmd.env("LC_ALL", "C");
    // No editor may open: every message comes from the app.
    cmd.env("GIT_EDITOR", "true");
    cmd.env("GIT_MERGE_AUTOEDIT", "no");
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

/// The command line for messages: `git` plus the arguments, quoted where needed.
pub fn display(args: &[&str]) -> String {
    let mut s = String::from("git");
    for a in args {
        s.push(' ');
        if a.is_empty() || a.contains([' ', '"', '\'', '\\', '*', '?', '~', '^', '$', '&', '|', ';', '<', '>', '(', ')']) {
            s.push('"');
            s.push_str(&a.replace('\\', "\\\\").replace('"', "\\\""));
            s.push('"');
        } else {
            s.push_str(a);
        }
    }
    s
}

/// Runs `git <args>` in `root` and waits for it. `stdin` is written to the process when given.
/// Returns stdout. A missing executable or a non-zero exit code is a [`GitError`].
pub fn run(root: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<String, GitError> {
    run_env(root, args, stdin, &[])
}

/// Like [`run`], with extra environment variables for the git process.
pub fn run_env(root: &Path, args: &[&str], stdin: Option<&[u8]>, env: &[(&str, &str)]) -> Result<String, GitError> {
    // Every call is a change that an action of the user started: it goes to the command log.
    let log = crate::cmdlog::begin(root, &display(args), crate::cmdlog::Source::App);
    let mut stderr = String::new();
    let result = run_inner(root, args, stdin, env, &mut stderr);
    crate::cmdlog::finish_run(log, &result, &stderr);
    result
}

/// `stderr_ok` gets what git wrote to stderr when the command succeeded.
fn run_inner(root: &Path, args: &[&str], stdin: Option<&[u8]>, env: &[(&str, &str)], stderr_ok: &mut String) -> Result<String, GitError> {
    let command = display(args);
    let fail = |code: Option<i32>, output: String| GitError { command: command.clone(), code, output };
    let mut cmd = self::command();
    cmd.current_dir(root).args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().map_err(|e| {
        let hint = if e.kind() == std::io::ErrorKind::NotFound {
            "Git was not found on PATH. Install Git (https://git-scm.com/downloads) and restart the app.".to_string()
        } else {
            format!("Cannot start git: {e}")
        };
        fail(None, hint)
    })?;
    if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A closed pipe means git exited early; its exit code and stderr tell why.
        let _ = pipe.write_all(data);
    }
    let out = child.wait_with_output().map_err(|e| fail(None, format!("Cannot read git output: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if out.status.success() {
        *stderr_ok = stderr.into_owned();
        return Ok(stdout);
    }
    let mut text = stderr.trim().to_string();
    if !stdout.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(stdout.trim());
    }
    Err(fail(out.status.code(), text))
}

/// Quotes a path for the POSIX shell that git uses to run editors and `exec` lines, also on
/// Windows. Backslashes become slashes there: the shell reads a backslash as an escape.
pub fn shell_quote(path: &Path) -> String {
    let text = path.display().to_string();
    let text = if cfg!(windows) { text.replace('\\', "/") } else { text };
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The argument that makes this executable copy a prepared file over the file git passes.
pub const APPLY_TODO: &str = "--apply-todo";

/// When the process was started by git as the sequence editor of an interactive rebase
/// (`<exe> --apply-todo <prepared> <git's todo file>`), copies the prepared todo list over
/// git's file and returns the exit code. Returns `None` in a normal start.
/// Both binaries call this first in `main`.
pub fn run_as_editor() -> Option<i32> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some(APPLY_TODO) {
        return None;
    }
    let (Some(prepared), Some(target)) = (args.get(2), args.get(3)) else {
        eprintln!("usage: {APPLY_TODO} <prepared file> <target file>");
        return Some(2);
    };
    match std::fs::copy(prepared, target) {
        Ok(_) => Some(0),
        Err(e) => {
            eprintln!("cannot write {target}: {e}");
            Some(1)
        }
    }
}

/// The executable that serves as sequence editor: this program, or the one named in
/// `GRAFTLINE_EDITOR_EXE` (tests point it at a built binary).
pub fn editor_exe() -> std::io::Result<std::path::PathBuf> {
    match std::env::var_os("GRAFTLINE_EDITOR_EXE") {
        Some(p) => Ok(p.into()),
        None => std::env::current_exe(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_handles_quotes_and_spaces() {
        assert_eq!(shell_quote(Path::new("/tmp/it's here/todo")), "'/tmp/it'\\''s here/todo'");
        if cfg!(windows) {
            assert_eq!(shell_quote(Path::new(r"C:\Program Files\App\app.exe")), "'C:/Program Files/App/app.exe'");
        }
    }

    #[test]
    fn display_quotes_arguments_with_spaces() {
        assert_eq!(display(&["commit", "-m", "two words"]), "git commit -m \"two words\"");
        assert_eq!(display(&["push", "--force-with-lease", "origin", "main"]), "git push --force-with-lease origin main");
        assert_eq!(display(&["tag", ""]), "git tag \"\"");
    }
}
