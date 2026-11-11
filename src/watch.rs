//! Watches a repository for changes made outside the app (terminal, IDE) and reports them
//! in batches, so the UI can refresh once after a burst of file events.

use notify::{EventKind, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Quiet time after the last event before a batch is reported.
const QUIET: Duration = Duration::from_millis(250);
/// A batch is reported after this time even when events keep coming.
const MAX_WAIT: Duration = Duration::from_millis(1500);
/// Working-tree paths kept per batch; more than this sets `many`.
const MAX_PATHS: usize = 64;

/// One batch of changes.
#[derive(Debug, Default, Clone)]
pub struct Change {
    /// Something inside the git directory changed (refs, HEAD, index, stashes).
    pub git: bool,
    /// Changed working-tree paths, relative to the repository root.
    pub paths: Vec<PathBuf>,
    /// More working-tree paths changed than `paths` holds.
    pub many: bool,
}

impl Change {
    pub fn tree(&self) -> bool {
        self.many || !self.paths.is_empty()
    }
}

enum Raw {
    Git,
    Tree(PathBuf),
}

/// Keeps the watch alive. Dropping it stops the watch and its thread.
pub struct RepoWatcher {
    _watcher: notify::RecommendedWatcher,
}

/// Tells whether a path inside the git directory matters. Lock files and new loose objects
/// change nothing the UI shows; the ref or index update that follows does.
fn git_path_matters(rel: &Path) -> bool {
    let first = rel.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
    if first == "objects" || first == "lfs" || first == "hooks" {
        return false;
    }
    !rel.extension().is_some_and(|e| e == "lock")
}

/// Starts watching `root` (recursively) and `git_dir` when it lies outside the root.
/// `on_change` runs on the watcher's own thread for every batch.
pub fn watch(root: &Path, git_dir: &Path, on_change: impl Fn(Change) + Send + 'static) -> notify::Result<RepoWatcher> {
    let (tx, rx) = mpsc::channel::<Raw>();
    let root_buf = root.to_path_buf();
    let git_buf = git_dir.to_path_buf();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        for path in event.paths {
            if let Ok(rel) = path.strip_prefix(&git_buf) {
                if git_path_matters(rel) {
                    let _ = tx.send(Raw::Git);
                }
            } else if let Ok(rel) = path.strip_prefix(&root_buf) {
                let _ = tx.send(Raw::Tree(rel.to_path_buf()));
            }
        }
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    if !git_dir.starts_with(root) {
        watcher.watch(git_dir, RecursiveMode::Recursive)?;
    }
    std::thread::Builder::new()
        .name("repo-watch".into())
        .spawn(move || {
            // The channel closes when the watcher is dropped; then the thread ends.
            while let Ok(first) = rx.recv() {
                let mut change = Change::default();
                let add = |raw: Raw, change: &mut Change| match raw {
                    Raw::Git => change.git = true,
                    Raw::Tree(p) => {
                        if change.paths.len() >= MAX_PATHS {
                            change.many = true;
                        } else if !change.paths.contains(&p) {
                            change.paths.push(p);
                        }
                    }
                };
                add(first, &mut change);
                let started = Instant::now();
                while started.elapsed() < MAX_WAIT {
                    match rx.recv_timeout(QUIET) {
                        Ok(raw) => add(raw, &mut change),
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
                on_change(change);
            }
        })
        .map_err(notify::Error::io)?;
    Ok(RepoWatcher { _watcher: watcher })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_files_and_objects_are_ignored() {
        assert!(!git_path_matters(Path::new("index.lock")));
        assert!(!git_path_matters(Path::new("objects/ab/cdef")));
        assert!(!git_path_matters(Path::new("refs/heads/main.lock")));
        assert!(git_path_matters(Path::new("index")));
        assert!(git_path_matters(Path::new("HEAD")));
        assert!(git_path_matters(Path::new("refs/heads/main")));
    }

    #[test]
    fn reports_a_batch_for_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::repo::clean_path(dir.path());
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let (tx, rx) = mpsc::channel();
        let _w = watch(&root, &root.join(".git"), move |c| {
            let _ = tx.send(c);
        })
        .unwrap();
        std::fs::write(root.join("a.txt"), "x").unwrap();
        std::fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let mut seen = Change::default();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !(seen.git && seen.tree()) {
            if let Ok(c) = rx.recv_timeout(Duration::from_millis(200)) {
                seen.git |= c.git;
                seen.paths.extend(c.paths);
                seen.many |= c.many;
            }
        }
        assert!(seen.git, "the HEAD change was not reported");
        assert!(seen.paths.iter().any(|p| p == Path::new("a.txt")), "a.txt was not reported: {:?}", seen.paths);
    }
}
