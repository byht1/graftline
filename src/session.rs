//! One open repository: its commit history, the cache of parsed commits and the file watch.

use crate::history::{self, CommitCache, History};
use crate::{repo, watch};
use anyhow::Result;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, RwLock};

/// The commit cache is written to disk after a build that read at least this many commits.
const SAVE_AFTER: usize = 64;

pub struct Session {
    pub id: String,
    /// Working directory, or the git directory of a bare repository.
    pub root: PathBuf,
    pub name: String,
    history: RwLock<Option<Arc<History>>>,
    cache: Mutex<CommitCache>,
    /// Where the commit cache is saved between launches.
    cache_file: PathBuf,
    /// Serializes history builds.
    build: tokio::sync::Mutex<()>,
    events: broadcast::Sender<String>,
    watcher: Mutex<Option<watch::RepoWatcher>>,
}

/// Stable id of a repository: a hash of its root path. The same repository gets the same id
/// at every launch, so saved tabs can refer to it.
pub fn repo_id(root: &Path) -> String {
    let mut text = root.display().to_string();
    if cfg!(windows) {
        text = text.to_lowercase();
    }
    // FNV-1a, 64 bit.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

impl Session {
    /// Opens the repository at `root` (already resolved by `repo::discover`) and starts
    /// watching it. Must run inside a tokio runtime.
    pub fn open(root: PathBuf, data_dir: &Path, events: broadcast::Sender<String>) -> Result<Arc<Session>> {
        let r = repo::open(&root)?;
        let git_dir = repo::clean_path(r.path());
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
        let name = name.strip_suffix(".git").filter(|n| !n.is_empty()).map(str::to_string).unwrap_or(name);
        let id = repo_id(&root);
        let s = Arc::new(Session {
            cache_file: data_dir.join("cache").join(format!("{id}.commits")),
            id,
            root,
            name,
            history: RwLock::new(None),
            cache: Mutex::new(CommitCache::default()),
            build: tokio::sync::Mutex::new(()),
            events,
            watcher: Mutex::new(None),
        });
        // The watcher holds a weak reference: closing the repository drops the session and the watch.
        let weak = Arc::downgrade(&s);
        let rt = tokio::runtime::Handle::current();
        match watch::watch(&s.root, &git_dir, move |change| {
            if let Some(s) = weak.upgrade() {
                rt.spawn(async move { s.on_fs_change(change).await });
            }
        }) {
            Ok(w) => *s.watcher.lock().unwrap() = Some(w),
            // Without a watch the app still works; the user refreshes by hand.
            Err(e) => tracing::warn!("cannot watch {}: {e}", s.root.display()),
        }
        Ok(s)
    }

    /// The current history; builds it on first use.
    pub async fn history(self: &Arc<Self>) -> Result<Arc<History>> {
        if let Some(h) = self.history.read().await.clone() {
            return Ok(h);
        }
        self.rebuild(false).await
    }

    /// Reads the refs again and rebuilds the history when they changed (or when `force`).
    /// Returns the current history.
    pub async fn rebuild(self: &Arc<Self>, force: bool) -> Result<Arc<History>> {
        let _guard = self.build.lock().await;
        let current = self.history.read().await.clone();
        let this = self.clone();
        let built = tokio::task::spawn_blocking(move || -> Result<Option<History>> {
            let r = repo::open(&this.root)?;
            let tips = history::read_tips(&r)?;
            let wip = false;
            if !force {
                if let Some(cur) = &current {
                    if cur.fingerprint == history::fingerprint(&tips, wip) {
                        return Ok(None);
                    }
                }
            }
            let mut cache = this.cache.lock().unwrap();
            if cache.is_empty() {
                // First build of this launch: start from the commits saved last time.
                *cache = CommitCache::load_file(&this.cache_file);
            }
            let h = history::build(&r, &mut cache, tips, wip)?;
            // Save when enough new commits were read to make the next launch faster.
            if h.read >= SAVE_AFTER {
                if let Err(e) = cache.save(&this.cache_file, Some(&h.oids())) {
                    tracing::warn!("cannot save the commit cache {}: {e:#}", this.cache_file.display());
                }
            }
            Ok(Some(h))
        })
        .await??;
        match built {
            Some(h) => {
                let h = Arc::new(h);
                *self.history.write().await = Some(h.clone());
                Ok(h)
            }
            None => Ok(self.history.read().await.clone().expect("history exists when unchanged")),
        }
    }

    fn emit(&self, value: serde_json::Value) {
        let _ = self.events.send(value.to_string());
    }

    /// Called by the watcher after a batch of file changes.
    async fn on_fs_change(self: Arc<Self>, change: watch::Change) {
        let before = self.history.read().await.as_ref().map(|h| h.fingerprint);
        let mut graph = false;
        if change.git && before.is_some() {
            match self.rebuild(false).await {
                Ok(h) => graph = Some(h.fingerprint) != before,
                Err(e) => tracing::warn!("refresh failed for {}: {e:#}", self.root.display()),
            }
        }
        self.emit(json!({ "type": "repo", "id": self.id, "git": change.git, "tree": change.tree(), "graph": graph }));
    }
}
