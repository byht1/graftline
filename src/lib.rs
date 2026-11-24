//! Graftline: a desktop Git client. This library is the core (Git logic, sessions, jobs);
//! the window is in `src/ui` and calls the core directly.

pub mod cmdlog;
pub mod conflicts;
pub mod extras;
pub mod gitcli;
pub mod graph;
pub mod history;
pub mod jobs;
pub mod ops;
pub mod prefs;
pub mod repo;
pub mod rewrite;
pub mod safety;
pub mod session;
pub mod update;
pub mod watch;
pub mod worktree;

use serde::{Deserialize, Serialize};
use session::Session;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

/// The product name. The package name in `Cargo.toml` and this constant are the two places
/// that define it for the Rust code.
pub const APP_NAME: &str = "Graftline";
/// Lowercase name for directories and files.
pub const APP_ID: &str = "graftline";

/// Options for building the application state.
pub struct Options {
    /// Directory for recent repositories, UI preferences and window state.
    pub data_dir: PathBuf,
    /// False in tests and tools: updates are then never downloaded or installed.
    pub desktop: bool,
}

pub fn default_data_dir() -> PathBuf {
    dirs::config_dir().map(|d| d.join(APP_ID)).unwrap_or_else(|| PathBuf::from(format!(".{APP_ID}")))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Recent {
    pub path: String,
    pub name: String,
    /// Seconds since the Unix epoch.
    pub opened_at: u64,
}

const MAX_RECENT: usize = 30;

pub const NO_GIT: &str = "Git was not found on PATH. Install Git (https://git-scm.com/downloads) and restart the app.";

/// Everything the window needs: open repositories, preferences, background jobs.
/// The async methods must run on the tokio runtime that the state was created on.
pub struct AppState {
    pub data_dir: PathBuf,
    sessions: RwLock<HashMap<String, Arc<Session>>>,
    recent: RwLock<Vec<Recent>>,
    prefs: RwLock<prefs::Prefs>,
    /// Core-to-UI events as JSON text: `{"type":"repo",…}` and `{"type":"job",…}`.
    pub events: broadcast::Sender<String>,
    pub git: Option<gitcli::GitInfo>,
    pub jobs: Arc<jobs::Jobs>,
    pub updater: Arc<update::Updater>,
}

pub type Shared = Arc<AppState>;

impl AppState {
    pub fn new(opts: Options) -> Shared {
        let (events, _) = broadcast::channel(256);
        let recent: Vec<Recent> =
            std::fs::read_to_string(opts.data_dir.join("recent.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Arc::new(AppState {
            prefs: RwLock::new(prefs::load(&prefs::store_path(&opts.data_dir))),
            updater: update::Updater::new(opts.desktop, opts.data_dir.clone()),
            data_dir: opts.data_dir,
            sessions: RwLock::new(HashMap::new()),
            recent: RwLock::new(recent),
            jobs: jobs::Jobs::new(events.clone()),
            events,
            git: gitcli::detect(),
        })
    }

    pub async fn session(&self, id: &str) -> anyhow::Result<Arc<Session>> {
        self.sessions.read().await.get(id).cloned().ok_or_else(|| anyhow::anyhow!("Repository not found"))
    }

    fn recent_path(&self) -> PathBuf {
        self.data_dir.join("recent.json")
    }

    async fn remember(&self, s: &Session) {
        let mut recent = self.recent.write().await;
        let path = s.root.display().to_string();
        recent.retain(|r| r.path != path);
        let opened_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        recent.insert(0, Recent { path, name: s.name.clone(), opened_at });
        recent.truncate(MAX_RECENT);
        if let Ok(text) = serde_json::to_string_pretty(&*recent) {
            let _ = prefs::write_atomic(&self.recent_path(), &text);
        }
    }

    /// Recent repositories, newest first; the flag tells whether the folder still exists.
    pub async fn recent(&self) -> Vec<(Recent, bool)> {
        self.recent.read().await.iter().map(|r| (r.clone(), std::path::Path::new(&r.path).exists())).collect()
    }

    pub async fn forget_recent(&self, path: &str) -> anyhow::Result<()> {
        let mut recent = self.recent.write().await;
        recent.retain(|r| r.path != path);
        prefs::write_atomic(&self.recent_path(), &serde_json::to_string_pretty(&*recent)?)
    }

    pub async fn prefs(&self) -> BTreeMap<String, String> {
        self.prefs.read().await.items.clone()
    }

    /// Stores one preference; `None` removes it.
    pub async fn set_pref(&self, key: &str, value: Option<String>) -> anyhow::Result<()> {
        let mut p = self.prefs.write().await;
        let mut items = p.items.clone();
        match value {
            Some(v) => {
                items.insert(key.to_string(), v);
            }
            None => {
                items.remove(key);
            }
        }
        p.items = prefs::check(items).map_err(|e| anyhow::anyhow!(e))?;
        p.rev += 1;
        prefs::save(&prefs::store_path(&self.data_dir), &p)
    }

    /// Opens the repository that contains `path`. A repository that is open already is reused.
    pub async fn open_repo(&self, path: &str) -> anyhow::Result<Arc<Session>> {
        let path = PathBuf::from(path.trim());
        let root = tokio::task::spawn_blocking(move || repo::discover(&path)).await??;
        let id = session::repo_id(&root);
        let existing = self.sessions.read().await.get(&id).cloned();
        let s = match existing {
            Some(s) => s,
            None => {
                let s = Session::open(root, &self.data_dir, self.events.clone())?;
                self.sessions.write().await.entry(id).or_insert(s).clone()
            }
        };
        self.remember(&s).await;
        Ok(s)
    }

    pub async fn close_repo(&self, id: &str) {
        self.sessions.write().await.remove(id);
    }

    /// Runs a blocking closure with an open repository of the session.
    pub async fn with_repo<T: Send + 'static>(
        &self,
        id: &str,
        f: impl FnOnce(&git2::Repository) -> anyhow::Result<T> + Send + 'static,
    ) -> anyhow::Result<T> {
        let s = self.session(id).await?;
        tokio::task::spawn_blocking(move || f(&repo::open(&s.root)?)).await?
    }

    /// Runs a change of the repository, then refreshes the session so the UI hears about it
    /// at once instead of waiting for the file watch.
    pub async fn mutate<T: Send + 'static>(
        &self,
        id: &str,
        f: impl FnOnce(&git2::Repository, &Session) -> anyhow::Result<T> + Send + 'static,
    ) -> anyhow::Result<T> {
        let s = self.session(id).await?;
        let s2 = s.clone();
        let result = tokio::task::spawn_blocking(move || f(&repo::open(&s2.root)?, &s2)).await;
        // Refresh also after a failure: a git command may have changed something before it failed.
        s.changed().await;
        result?
    }

    /// Cherry-pick, revert, reset, interactive rebase and the steps of an operation in progress.
    pub async fn run_history(&self, id: &str, op: rewrite::HistoryOp) -> anyhow::Result<serde_json::Value> {
        if self.git.is_none() {
            anyhow::bail!(NO_GIT);
        }
        self.mutate(id, move |r, s| rewrite::run(r, &s.root, op)).await
    }

    /// Starts a network operation. The answer comes at once; progress and the result arrive
    /// as `job` events.
    pub async fn start_job(&self, id: &str, job: ops::NetJob) -> anyhow::Result<jobs::JobInfo> {
        let s = self.session(id).await?;
        let (title, args) = job.command();
        let rt = tokio::runtime::Handle::current();
        let session = s.clone();
        Ok(self.jobs.start(Some(id.to_string()), &title, s.root.clone(), None, args, move |_ok| {
            // Refresh in both cases: a failed pull can still have fetched new commits.
            rt.spawn(async move { session.changed().await });
        })?)
    }

    /// Runs a command line that the user typed in the terminal panel: one git command, no
    /// shell. Returns the id of its entry in the command log; the output arrives there.
    pub async fn run_command(&self, id: &str, line: &str) -> anyhow::Result<u64> {
        let s = self.session(id).await?;
        // A line that cannot run is answered in the terminal, like a shell does.
        if self.git.is_none() {
            return Ok(cmdlog::reject(&s.root, line.trim(), NO_GIT));
        }
        let args = match cmdlog::parse_line(line) {
            Ok(args) => args,
            Err(reason) => return Ok(cmdlog::reject(&s.root, line.trim(), &reason)),
        };
        let rt = tokio::runtime::Handle::current();
        let session = s.clone();
        Ok(cmdlog::run_user(&s.root, args, move || {
            // The command may have changed anything: read the repository again.
            rt.spawn(async move { session.changed().await });
        }))
    }

    /// Starts `git clone` of `url` into a new folder inside `parent`. Returns the job and the
    /// path of the new repository. `name` is taken from the URL when absent.
    pub fn clone_repo(&self, url: &str, parent: &str, name: Option<&str>) -> anyhow::Result<(jobs::JobInfo, PathBuf)> {
        let url = url.trim().to_string();
        if url.is_empty() {
            anyhow::bail!("Enter the URL of the repository");
        }
        let parent = PathBuf::from(parent.trim());
        if !parent.is_dir() {
            anyhow::bail!("{} is not a folder", parent.display());
        }
        let name = name.map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| ops::clone_dir_name(&url));
        if name.contains(['/', '\\']) || name == "." || name == ".." {
            anyhow::bail!("'{name}' is not a folder name");
        }
        let target = parent.join(&name);
        if target.exists() {
            anyhow::bail!("{} already exists", target.display());
        }
        let args = vec!["clone".to_string(), "--progress".to_string(), url, name];
        // The clone runs in the parent folder; its log entry belongs to the new repository.
        let info = self.jobs.start(None, "Clone", parent, Some(target.clone()), args, |_| {})?;
        Ok((info, target))
    }
}
