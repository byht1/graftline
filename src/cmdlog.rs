//! The command log behind the terminal panel: every git command the app runs for a
//! repository, the equivalent command of each change made with the built-in Git library,
//! and the commands the user types. The log is kept in memory for the life of the process
//! and is written whether or not the panel is open.

use crate::gitcli::{self, GitError};
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;
use tokio::sync::broadcast;

/// Entries kept for one repository; the oldest finished entry goes first.
const MAX_ENTRIES: usize = 300;
/// Output kept for one command.
const MAX_OUTPUT: usize = 256 * 1024;
/// Paths written in full in the equivalent command of a built-in operation.
const MAX_PATHS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The app ran the system `git` for an action in the interface.
    App,
    /// The built-in Git library did the work; the entry shows the equivalent command.
    BuiltIn,
    /// The user typed the command in the terminal panel.
    User,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: u64,
    /// The command line as the user could type it.
    pub command: String,
    pub source: Source,
    /// What git printed, with `\n` line ends and without terminal escape codes.
    pub output: String,
    /// True when `output` lost its end to the size limit.
    pub truncated: bool,
    pub running: bool,
    pub ok: bool,
    pub code: Option<i32>,
    pub cancelled: bool,
    /// Run time in milliseconds, when the command ended.
    pub ms: Option<u64>,
}

struct Slot {
    key: String,
    entry: Entry,
    started: Instant,
}

#[derive(Default)]
struct Log {
    slots: VecDeque<Slot>,
    next: u64,
    /// Process ids of the user's running commands.
    pids: HashMap<u64, u32>,
}

static LOG: LazyLock<Mutex<Log>> = LazyLock::new(Default::default);
static EVENTS: LazyLock<broadcast::Sender<String>> = LazyLock::new(|| broadcast::channel(512).0);

/// The name of a repository in the log: its root path in one spelling.
pub fn key(dir: &Path) -> String {
    let text = dir.display().to_string().replace('\\', "/");
    let text = text.trim_end_matches('/').to_string();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

/// Keys of the repositories whose log changed. A receiver that lags must read all again.
pub fn subscribe() -> broadcast::Receiver<String> {
    EVENTS.subscribe()
}

fn changed(key: &str) {
    let _ = EVENTS.send(key.to_string());
}

/// Adds a running command and returns its id.
pub fn begin(dir: &Path, command: &str, source: Source) -> u64 {
    let key = key(dir);
    let id = {
        let mut log = LOG.lock().unwrap();
        log.next += 1;
        let id = log.next;
        let entry = Entry {
            id,
            command: command.to_string(),
            source,
            output: String::new(),
            truncated: false,
            running: true,
            ok: false,
            code: None,
            cancelled: false,
            ms: None,
        };
        log.slots.push_back(Slot { key: key.clone(), entry, started: Instant::now() });
        if log.slots.iter().filter(|s| s.key == key).count() > MAX_ENTRIES {
            if let Some(at) = log.slots.iter().position(|s| s.key == key && !s.entry.running) {
                log.slots.remove(at);
            }
        }
        id
    };
    changed(&key);
    id
}

/// Removes terminal escape sequences (colors, cursor moves) and carriage returns.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    // Parameter bytes, then one final byte in `@`..`~`.
                    for f in chars.by_ref() {
                        if ('@'..='~').contains(&f) {
                            break;
                        }
                    }
                }
            }
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// Adds text to the output of a command. A line break is put between two pieces.
pub fn append(id: u64, text: &str) {
    let text = plain(text);
    let text = text.trim_end_matches('\n');
    let key = {
        let mut log = LOG.lock().unwrap();
        let Some(slot) = log.slots.iter_mut().find(|s| s.entry.id == id) else { return };
        let e = &mut slot.entry;
        if e.truncated {
            return;
        }
        if !e.output.is_empty() {
            e.output.push('\n');
        }
        let room = MAX_OUTPUT.saturating_sub(e.output.len());
        if text.len() > room {
            let mut cut = room;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            e.output.push_str(&text[..cut]);
            e.truncated = true;
        } else {
            e.output.push_str(text);
        }
        slot.key.clone()
    };
    changed(&key);
}

/// Marks a command as ended. `code` is absent when git did not start or was stopped.
pub fn finish(id: u64, code: Option<i32>, ok: bool) {
    let key = {
        let mut log = LOG.lock().unwrap();
        log.pids.remove(&id);
        let Some(slot) = log.slots.iter_mut().find(|s| s.entry.id == id) else { return };
        slot.entry.running = false;
        slot.entry.ok = ok;
        slot.entry.code = code;
        slot.entry.ms = Some(slot.started.elapsed().as_millis() as u64);
        slot.key.clone()
    };
    changed(&key);
}

/// Records a change that the built-in Git library made: `args` is the git command that
/// does the same.
pub fn note(repo: &git2::Repository, args: &[&str]) {
    note_text(repo, gitcli::display(args));
}

/// Like [`note`] for a command that ends with a list of paths. A long list is cut.
pub fn note_paths(repo: &git2::Repository, args: &[&str], paths: &[String]) {
    let mut all: Vec<&str> = args.to_vec();
    all.extend(paths.iter().take(MAX_PATHS).map(String::as_str));
    let mut text = gitcli::display(&all);
    if paths.len() > MAX_PATHS {
        text.push_str(&format!("  # and {} more", paths.len() - MAX_PATHS));
    }
    note_text(repo, text);
}

fn note_text(repo: &git2::Repository, command: String) {
    let dir = repo.workdir().unwrap_or_else(|| repo.path());
    let id = begin(dir, &command, Source::BuiltIn);
    finish(id, Some(0), true);
}

/// The log of one repository, oldest entry first.
pub fn list(dir: &Path) -> Vec<Entry> {
    let key = key(dir);
    LOG.lock().unwrap().slots.iter().filter(|s| s.key == key).map(|s| s.entry.clone()).collect()
}

/// Removes the finished entries of one repository.
pub fn clear(dir: &Path) {
    let key = key(dir);
    LOG.lock().unwrap().slots.retain(|s| s.key != key || s.entry.running);
    changed(&key);
}

// ---------------------------------------------------------------- commands the user types

/// Splits a command line into the arguments for git. Quotes work as in a POSIX shell:
/// single quotes keep everything, double quotes know `\"` and `\\`. A leading `git` is
/// optional. Pipes, redirects and command chains are refused: no shell runs the line.
pub fn parse_line(line: &str) -> Result<Vec<String>, String> {
    let mut args: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    // Set while the argument has no quoted part: only then can it be a shell operator.
    let mut bare = true;
    let mut chars = line.trim().chars().peekable();
    let push = |cur: &mut String, has: &mut bool, bare: &mut bool, args: &mut Vec<String>| -> Result<(), String> {
        if *has {
            if *bare && matches!(cur.as_str(), "|" | "||" | "&&" | "&" | ";" | ">" | ">>" | "<") {
                return Err(format!("'{cur}' is not supported: the line is one git command, not a shell script"));
            }
            args.push(std::mem::take(cur));
        }
        *has = false;
        *bare = true;
        Ok(())
    };
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => push(&mut cur, &mut has, &mut bare, &mut args)?,
            '\'' => {
                has = true;
                bare = false;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => return Err("A single quote is not closed".into()),
                    }
                }
            }
            '"' => {
                has = true;
                bare = false;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') if matches!(chars.peek(), Some('"') | Some('\\')) => cur.push(chars.next().unwrap_or('\\')),
                        Some(c) => cur.push(c),
                        None => return Err("A double quote is not closed".into()),
                    }
                }
            }
            c => {
                has = true;
                cur.push(c);
            }
        }
    }
    push(&mut cur, &mut has, &mut bare, &mut args)?;
    if args.first().map(String::as_str) == Some("git") {
        args.remove(0);
    }
    if args.is_empty() {
        return Err("Type a git command, for example: status".into());
    }
    Ok(args)
}

/// Records a line of the user that did not run, with the reason.
pub fn reject(dir: &Path, line: &str, reason: &str) -> u64 {
    let id = begin(dir, line, Source::User);
    append(id, reason);
    finish(id, None, false);
    id
}

/// Copies what a process prints into the log entry. A line that ends with a carriage
/// return only is a progress line that git draws again; the last such line is dropped.
fn pump(mut from: impl Read, id: u64) {
    let mut buf = [0u8; 8192];
    let mut line: Vec<u8> = Vec::new();
    // True after a carriage return: the next byte decides what the line was.
    let mut held = false;
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let mut ready: Vec<u8> = Vec::new();
        for &b in &buf[..n] {
            match b {
                b'\n' => {
                    ready.extend_from_slice(&line);
                    ready.push(b'\n');
                    line.clear();
                    held = false;
                }
                b'\r' => held = true,
                b => {
                    if held {
                        line.clear();
                        held = false;
                    }
                    line.push(b);
                }
            }
        }
        if !ready.is_empty() {
            // The piece ends with a line break that `append` puts back between pieces.
            ready.pop();
            append(id, &String::from_utf8_lossy(&ready));
        }
    }
    if !line.is_empty() {
        append(id, &String::from_utf8_lossy(&line));
    }
}

/// Starts `git <args>` in `dir` for the user and returns the id of the log entry. The
/// output arrives in the entry while git runs. `on_done` runs on a worker thread at the end.
pub fn run_user(dir: &Path, args: Vec<String>, on_done: impl FnOnce() + Send + 'static) -> u64 {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let id = begin(dir, &gitcli::display(&refs), Source::User);
    let mut cmd = gitcli::command();
    cmd.current_dir(dir).args(&args).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            append(
                id,
                &if e.kind() == std::io::ErrorKind::NotFound { crate::NO_GIT.to_string() } else { format!("Cannot start git: {e}") },
            );
            finish(id, None, false);
            on_done();
            return id;
        }
    };
    LOG.lock().unwrap().pids.insert(id, child.id());
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let spawned = std::thread::Builder::new().name(format!("git-user-{id}")).spawn(move || {
        let err_thread = std::thread::spawn(move || {
            if let Some(s) = stderr {
                pump(s, id);
            }
        });
        if let Some(s) = stdout {
            pump(s, id);
        }
        let _ = err_thread.join();
        let status = child.wait();
        let code = status.as_ref().ok().and_then(|s| s.code());
        finish(id, code, status.map(|s| s.success()).unwrap_or(false));
        on_done();
    });
    if let Err(e) = spawned {
        append(id, &format!("Cannot start a thread: {e}"));
        finish(id, None, false);
    }
    id
}

/// Stops a running command of the user. Returns false when it is not running.
pub fn cancel(id: u64) -> bool {
    let pid = {
        let mut log = LOG.lock().unwrap();
        let Some(pid) = log.pids.get(&id).copied() else { return false };
        if let Some(slot) = log.slots.iter_mut().find(|s| s.entry.id == id) {
            slot.entry.cancelled = true;
        }
        pid
    };
    crate::jobs::kill_tree(pid);
    true
}

/// Writes the result of a finished `git` call of the app into its entry.
pub(crate) fn finish_run(id: u64, result: &Result<String, GitError>, stderr: &str) {
    match result {
        Ok(stdout) => {
            for part in [stderr, stdout.as_str()] {
                if !part.trim().is_empty() {
                    append(id, part.trim_end());
                }
            }
            finish(id, Some(0), true);
        }
        Err(e) => {
            if !e.output.is_empty() {
                append(id, &e.output);
            }
            finish(id, e.code, false);
        }
    }
}

/// Waits until a command ends and returns its entry (the running entry after `timeout`).
pub fn wait(id: u64, timeout: std::time::Duration) -> Option<Entry> {
    let end = Instant::now() + timeout;
    loop {
        let found = LOG.lock().unwrap().slots.iter().find(|s| s.entry.id == id).map(|s| s.entry.clone());
        match found {
            Some(e) if !e.running => return Some(e),
            None => return None,
            _ if Instant::now() >= end => return found,
            _ => std::thread::sleep(std::time::Duration::from_millis(15)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_split_like_a_shell() {
        assert_eq!(parse_line("git status").unwrap(), ["status"]);
        assert_eq!(parse_line("  log  --oneline -n 5 ").unwrap(), ["log", "--oneline", "-n", "5"]);
        assert_eq!(
            parse_line(r#"commit -m "two words" --author='A B <a@b.c>'"#).unwrap(),
            ["commit", "-m", "two words", "--author=A B <a@b.c>"]
        );
        assert_eq!(parse_line(r#"commit -m "say \"hi\" \\ there""#).unwrap(), ["commit", "-m", r#"say "hi" \ there"#]);
        assert_eq!(parse_line(r#"tag """#).unwrap(), ["tag", ""]);
        assert_eq!(parse_line(r"add C:\dir\file.txt").unwrap(), ["add", r"C:\dir\file.txt"]);
    }

    #[test]
    fn shell_syntax_is_refused() {
        assert!(parse_line("").is_err());
        assert!(parse_line("git").is_err());
        assert!(parse_line("log | head").is_err());
        assert!(parse_line("status && git push").is_err());
        assert!(parse_line("log > out.txt").is_err());
        assert!(parse_line("commit -m \"open").is_err());
        // A quoted operator is an ordinary argument.
        assert_eq!(parse_line("commit -m '|'").unwrap(), ["commit", "-m", "|"]);
    }

    #[test]
    fn escape_codes_are_removed() {
        assert_eq!(plain("\u{1b}[31mred\u{1b}[m plain\r"), "red plain");
    }

    #[test]
    fn progress_lines_are_dropped_and_crlf_is_one_break() {
        let dir = tempfile::tempdir().unwrap();
        let id = begin(dir.path(), "git test", Source::App);
        pump(&b"one\r\ntwo\nRecv 10%\rRecv 90%\rdone\nlast"[..], id);
        finish(id, Some(0), true);
        let log = list(dir.path());
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].output, "one\ntwo\ndone\nlast");
        assert!(!log[0].running && log[0].ok);
    }

    #[test]
    fn output_has_a_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let id = begin(dir.path(), "git big", Source::User);
        append(id, &"x".repeat(MAX_OUTPUT + 10));
        append(id, "more");
        let e = &list(dir.path())[0];
        assert!(e.truncated);
        assert_eq!(e.output.len(), MAX_OUTPUT);
    }

    #[test]
    fn old_entries_leave_and_clear_keeps_running_ones() {
        let dir = tempfile::tempdir().unwrap();
        let first = begin(dir.path(), "git first", Source::App);
        for i in 0..MAX_ENTRIES + 5 {
            let id = begin(dir.path(), &format!("git n{i}"), Source::BuiltIn);
            finish(id, Some(0), true);
        }
        let log = list(dir.path());
        assert_eq!(log.len(), MAX_ENTRIES);
        // The running entry stays although it is the oldest.
        assert_eq!(log[0].id, first);
        clear(dir.path());
        assert_eq!(list(dir.path()).len(), 1);
        finish(first, Some(0), true);
    }
}
