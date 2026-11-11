//! Long-running git commands (fetch, pull, push, clone). A job runs `git` in its own thread,
//! reports progress to the UI as events and can be cancelled. The HTTP request that starts a
//! job returns at once, so no git operation blocks the interface.

use crate::gitcli;
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobInfo {
    pub id: String,
    /// The repository the job belongs to; `None` for a clone.
    pub repo: Option<String>,
    /// Short text for the UI, for example "Fetch".
    pub title: String,
    pub command: String,
    /// The last line git printed.
    pub line: String,
    pub percent: Option<u8>,
    /// `running`, `done`, `failed` or `cancelled`.
    pub state: &'static str,
    /// Everything git printed, when the job failed.
    pub output: Option<String>,
}

struct Running {
    pid: u32,
    cancelled: Arc<AtomicBool>,
    info: JobInfo,
}

pub struct Jobs {
    running: Mutex<HashMap<String, Running>>,
    events: broadcast::Sender<String>,
    next: AtomicU64,
}

/// Reads "45%" from a progress line such as `Receiving objects:  45% (450/1000)`.
pub fn parse_percent(line: &str) -> Option<u8> {
    let at = line.find('%')?;
    let digits: String = line[..at].chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
    digits.parse::<u8>().ok().filter(|p| *p <= 100)
}

/// Stops a process and the processes it started (git runs helpers for the network).
fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000)
            .status();
    }
    #[cfg(not(windows))]
    {
        // The job is the leader of its own process group: the negative id addresses the group.
        let _ = std::process::Command::new("kill").args(["-TERM", &format!("-{pid}")]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
}

impl Jobs {
    pub fn new(events: broadcast::Sender<String>) -> Arc<Jobs> {
        Arc::new(Jobs { running: Mutex::new(HashMap::new()), events, next: AtomicU64::new(1) })
    }

    fn emit(&self, info: &JobInfo) {
        let _ = self.events.send(json!({ "type": "job", "job": info }).to_string());
    }

    pub fn list(&self) -> Vec<JobInfo> {
        self.running.lock().unwrap().values().map(|r| r.info.clone()).collect()
    }

    /// Asks a running job to stop. Returns false when there is no such job.
    pub fn cancel(&self, id: &str) -> bool {
        let running = self.running.lock().unwrap();
        match running.get(id) {
            Some(job) => {
                job.cancelled.store(true, Ordering::SeqCst);
                kill_tree(job.pid);
                true
            }
            None => false,
        }
    }

    /// Starts `git <args>` in `dir`. `on_done(success)` runs on the job's thread when git exits.
    pub fn start(
        self: &Arc<Self>,
        repo: Option<String>,
        title: &str,
        dir: PathBuf,
        args: Vec<String>,
        on_done: impl FnOnce(bool) + Send + 'static,
    ) -> Result<JobInfo, gitcli::GitError> {
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let command = gitcli::display(&arg_refs);
        let mut cmd = gitcli::command();
        cmd.current_dir(&dir).args(&args).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd.spawn().map_err(|e| gitcli::GitError {
            command: command.clone(),
            code: None,
            output: if e.kind() == std::io::ErrorKind::NotFound {
                "Git was not found on PATH. Install Git (https://git-scm.com/downloads) and restart the app.".into()
            } else {
                format!("Cannot start git: {e}")
            },
        })?;
        let id = format!("job{}", self.next.fetch_add(1, Ordering::SeqCst));
        let info = JobInfo {
            id: id.clone(),
            repo,
            title: title.to_string(),
            command,
            line: String::new(),
            percent: None,
            state: "running",
            output: None,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        self.running.lock().unwrap().insert(id.clone(), Running { pid: child.id(), cancelled: cancelled.clone(), info: info.clone() });
        self.emit(&info);

        let jobs = self.clone();
        let started = info.clone();
        std::thread::Builder::new()
            .name(format!("git-{id}"))
            .spawn(move || {
                let mut info = started;
                // stdout is small for these commands; a second thread keeps its pipe from filling.
                let stdout = child.stdout.take();
                let out_thread = std::thread::spawn(move || {
                    let mut text = String::new();
                    if let Some(mut s) = stdout {
                        let _ = s.read_to_string(&mut text);
                    }
                    text
                });
                // git writes progress to stderr and redraws a line with a carriage return.
                let mut log: Vec<String> = Vec::new();
                let mut current: Vec<u8> = Vec::new();
                let mut last_emit = Instant::now() - Duration::from_secs(1);
                if let Some(mut stderr) = child.stderr.take() {
                    let mut buf = [0u8; 4096];
                    loop {
                        let n = match stderr.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        for &b in &buf[..n] {
                            if b != b'\r' && b != b'\n' {
                                current.push(b);
                                continue;
                            }
                            let line = String::from_utf8_lossy(&current).trim_end().to_string();
                            current.clear();
                            if line.is_empty() {
                                continue;
                            }
                            info.percent = parse_percent(&line);
                            info.line = line.clone();
                            // A finished line stays in the log; a redrawn one is replaced.
                            if b == b'\n' {
                                log.push(line);
                            }
                            if b == b'\n' || last_emit.elapsed() >= Duration::from_millis(80) {
                                last_emit = Instant::now();
                                if let Some(r) = jobs.running.lock().unwrap().get_mut(&info.id) {
                                    r.info = info.clone();
                                }
                                jobs.emit(&info);
                            }
                        }
                    }
                    if !current.is_empty() {
                        log.push(String::from_utf8_lossy(&current).trim_end().to_string());
                    }
                }
                let status = child.wait();
                let stdout = out_thread.join().unwrap_or_default();
                let ok = status.as_ref().map(|s| s.success()).unwrap_or(false);
                info.state = if ok {
                    "done"
                } else if cancelled.load(Ordering::SeqCst) {
                    "cancelled"
                } else {
                    "failed"
                };
                info.percent = None;
                if !ok {
                    let mut text = log.join("\n");
                    if !stdout.trim().is_empty() {
                        text.push('\n');
                        text.push_str(stdout.trim());
                    }
                    info.output = Some(text.trim().to_string());
                } else if let Some(last) = log.last() {
                    info.line = last.clone();
                }
                jobs.running.lock().unwrap().remove(&info.id);
                // The callback refreshes the repository before the UI hears that the job ended.
                on_done(ok);
                jobs.emit(&info);
            })
            .map_err(|e| gitcli::GitError { command: info.command.clone(), code: None, output: format!("Cannot start a thread: {e}") })?;
        Ok(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_is_read_from_progress_lines() {
        assert_eq!(parse_percent("Receiving objects:  45% (450/1000), 1.2 MiB | 800 KiB/s"), Some(45));
        assert_eq!(parse_percent("remote: Compressing objects: 100% (12/12), done."), Some(100));
        assert_eq!(parse_percent("Resolving deltas:   7% (1/14)"), Some(7));
        assert_eq!(parse_percent("From https://example.com/repo"), None);
        assert_eq!(parse_percent("weird 999% value"), None);
    }
}
