//! Graftline: a desktop Git client (local server + embedded web UI).

pub mod conflicts;
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
pub mod watch;
pub mod worktree;

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use session::Session;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

/// The product name. The package name in `Cargo.toml` and this constant are the two places
/// that define it for the Rust code.
pub const APP_NAME: &str = "Graftline";
/// Lowercase name for directories, files and cookies.
pub const APP_ID: &str = "graftline";

/// Options for building the HTTP application.
pub struct Options {
    /// Directory for recent repositories, UI preferences and window state.
    pub data_dir: PathBuf,
    /// Desktop mode: a per-launch secret. Only the app window, which receives it, can use the API.
    pub app_token: Option<String>,
    /// Repositories to open on start (command line).
    pub open: Vec<PathBuf>,
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

pub struct AppState {
    pub data_dir: PathBuf,
    pub app_token: Option<String>,
    sessions: RwLock<HashMap<String, Arc<Session>>>,
    recent: RwLock<Vec<Recent>>,
    prefs: RwLock<prefs::Prefs>,
    /// Server-to-UI events as JSON text.
    pub events: broadcast::Sender<String>,
    git: Option<gitcli::GitInfo>,
    jobs: Arc<jobs::Jobs>,
    /// Repositories given on the command line; the UI opens them once.
    startup: RwLock<Vec<String>>,
}

pub type Shared = Arc<AppState>;

pub struct ApiError(StatusCode, Value);

impl ApiError {
    pub fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        ApiError(status, json!({ "error": msg.into() }))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        let e: anyhow::Error = e.into();
        // A failed git command is shown word for word, together with the command.
        if let Some(git) = e.downcast_ref::<gitcli::GitError>() {
            return ApiError(
                StatusCode::BAD_REQUEST,
                json!({ "error": git.to_string(), "command": git.command, "output": git.output, "code": git.code }),
            );
        }
        ApiError::new(StatusCode::BAD_REQUEST, format!("{e:#}"))
    }
}

fn not_found(what: &str) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, format!("{what} not found"))
}

type ApiResult<T> = Result<T, ApiError>;

impl AppState {
    async fn session(&self, id: &str) -> ApiResult<Arc<Session>> {
        self.sessions.read().await.get(id).cloned().ok_or_else(|| not_found("Repository"))
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

    /// Runs a blocking closure with an open repository of the session.
    async fn with_repo<T: Send + 'static>(
        &self,
        id: &str,
        f: impl FnOnce(&git2::Repository) -> anyhow::Result<T> + Send + 'static,
    ) -> ApiResult<T> {
        let s = self.session(id).await?;
        Ok(tokio::task::spawn_blocking(move || f(&repo::open(&s.root)?)).await.map_err(|e| anyhow::anyhow!("task failed: {e}"))??)
    }
}

// ---------- static files ----------

/// The UI files. Debug builds read them from disk so a page reload shows UI changes;
/// release builds serve the copies embedded in the binary.
macro_rules! assets {
    ($($name:literal => $mime:literal),* $(,)?) => {
        const ASSETS: &[(&str, &str, &[u8])] = &[
            $(($name, $mime, include_bytes!(concat!("../static/", $name)).as_slice())),*
        ];
    };
}

assets! {
    "style.css" => "text/css; charset=utf-8",
    "app.js" => "text/javascript; charset=utf-8",
    "ui.js" => "text/javascript; charset=utf-8",
    "graph.js" => "text/javascript; charset=utf-8",
    "diff.js" => "text/javascript; charset=utf-8",
    "changes.js" => "text/javascript; charset=utf-8",
    "actions.js" => "text/javascript; charset=utf-8",
    "history.js" => "text/javascript; charset=utf-8",
    "highlight.js" => "text/javascript; charset=utf-8",
    "favicon.svg" => "image/svg+xml",
    "brand/symbol.svg" => "image/svg+xml",
}

async fn asset(Path(path): Path<String>) -> Response {
    let Some((name, mime, embedded)) = ASSETS.iter().find(|(n, _, _)| *n == path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let headers = [(header::CONTENT_TYPE, *mime), (header::CACHE_CONTROL, "no-cache")];
    #[cfg(debug_assertions)]
    if let Ok(body) = std::fs::read(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/static")).join(name)) {
        return (headers, body).into_response();
    }
    let _ = name;
    (headers, *embedded).into_response()
}

async fn index_page(State(st): State<Shared>) -> Response {
    #[cfg(debug_assertions)]
    let html = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/static/index.html"))
        .unwrap_or_else(|_| include_str!("../static/index.html").to_string());
    #[cfg(not(debug_assertions))]
    let html = include_str!("../static/index.html").to_string();
    let html = if st.app_token.is_some() { prefs::inject(&html, &*st.prefs.read().await) } else { html };
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], html).into_response()
}

// ---------- handlers: app ----------

async fn info(State(st): State<Shared>) -> Json<Value> {
    let startup = std::mem::take(&mut *st.startup.write().await);
    Json(json!({
        "name": APP_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "desktop": st.app_token.is_some(),
        "git": st.git,
        "os": std::env::consts::OS,
        "startup": startup,
    }))
}

async fn get_prefs(State(st): State<Shared>) -> Json<prefs::Prefs> {
    Json(st.prefs.read().await.clone())
}

#[derive(Deserialize)]
struct PrefsUpdate {
    items: std::collections::BTreeMap<String, String>,
}

async fn put_prefs(State(st): State<Shared>, Json(req): Json<PrefsUpdate>) -> ApiResult<Json<Value>> {
    let items = prefs::check(req.items).map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e))?;
    let mut p = st.prefs.write().await;
    p.rev += 1;
    p.items = items;
    prefs::save(&prefs::store_path(&st.data_dir), &p)?;
    Ok(Json(json!({ "rev": p.rev })))
}

async fn event_stream(State(st): State<Shared>) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let stream = BroadcastStream::new(st.events.subscribe()).filter_map(|m| m.ok().map(|text| Ok(Event::default().data(text))));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn list_recent(State(st): State<Shared>) -> Json<Value> {
    let recent = st.recent.read().await;
    let items: Vec<Value> = recent
        .iter()
        .map(|r| json!({ "path": r.path, "name": r.name, "openedAt": r.opened_at, "exists": std::path::Path::new(&r.path).exists() }))
        .collect();
    Json(json!(items))
}

#[derive(Deserialize)]
struct PathBody {
    path: String,
}

async fn forget_recent(State(st): State<Shared>, Json(req): Json<PathBody>) -> ApiResult<Json<Value>> {
    let mut recent = st.recent.write().await;
    recent.retain(|r| r.path != req.path);
    prefs::write_atomic(&st.recent_path(), &serde_json::to_string_pretty(&*recent)?)?;
    Ok(Json(json!({ "ok": true })))
}

/// Desktop mode: the native "choose a folder" dialog. Returns the path, or `cancelled`.
async fn pick_folder(State(st): State<Shared>) -> ApiResult<Json<Value>> {
    if st.app_token.is_none() {
        return Err(not_found("Endpoint"));
    }
    #[cfg(feature = "desktop")]
    {
        let picked = tokio::task::spawn_blocking(|| {
            // The async dialog hands the panel to the UI (main) thread where the OS needs it
            // (macOS); this worker thread only waits for the answer.
            futures::executor::block_on(rfd::AsyncFileDialog::new().set_title("Open a repository").pick_folder())
                .map(|h| h.path().to_path_buf())
        })
        .await
        .map_err(|e| anyhow::anyhow!("dialog failed: {e}"))?;
        Ok(Json(match picked {
            Some(p) => json!({ "path": p.display().to_string() }),
            None => json!({ "cancelled": true }),
        }))
    }
    #[cfg(not(feature = "desktop"))]
    Err(not_found("Endpoint"))
}

// ---------- handlers: repositories ----------

fn session_json(s: &Session) -> Value {
    json!({ "id": s.id, "path": s.root.display().to_string(), "name": s.name })
}

async fn open_repo(State(st): State<Shared>, Json(req): Json<PathBody>) -> ApiResult<Json<Value>> {
    let path = PathBuf::from(req.path.trim());
    let root = tokio::task::spawn_blocking(move || repo::discover(&path)).await.map_err(|e| anyhow::anyhow!("task failed: {e}"))??;
    let id = session::repo_id(&root);
    let existing = st.sessions.read().await.get(&id).cloned();
    let s = match existing {
        Some(s) => s,
        None => {
            let s = Session::open(root, &st.data_dir, st.events.clone())?;
            st.sessions.write().await.entry(id).or_insert(s).clone()
        }
    };
    st.remember(&s).await;
    Ok(Json(session_json(&s)))
}

async fn close_repo(State(st): State<Shared>, Path(id): Path<String>) -> Json<Value> {
    st.sessions.write().await.remove(&id);
    Json(json!({ "ok": true }))
}

async fn get_refs(State(st): State<Shared>, Path(id): Path<String>) -> ApiResult<Json<repo::Refs>> {
    Ok(Json(st.with_repo(&id, repo::refs).await?))
}

#[derive(Deserialize)]
struct PageQuery {
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
}

pub fn graph_rows(h: &history::History, offset: usize, limit: usize) -> Vec<Value> {
    let end = offset.saturating_add(limit).min(h.commits.len());
    let mut rows = Vec::with_capacity(end.saturating_sub(offset));
    for (i, g) in h.layout.rows(offset, end).into_iter().enumerate() {
        let row = offset + i;
        let c = &h.commits[row];
        let (name, email) = h.author(c.author);
        // Edges as a flat list: kind, column, color, …
        let edges: Vec<u16> = g.edges.iter().flat_map(|e| [e.kind as u16, e.col, e.color as u16]).collect();
        rows.push(json!({
            "o": if c.kind == history::Kind::Wip { String::new() } else { c.oid.to_string() },
            "k": c.kind,
            "c": g.col,
            "cl": g.color,
            "e": edges,
            "p": h.parents[row],
            "s": &*c.summary,
            "a": name,
            "ae": email,
            "t": c.time,
            "r": h.labels.get(&c.oid),
        }));
    }
    rows
}

async fn get_graph(State(st): State<Shared>, Path(id): Path<String>, Query(q): Query<PageQuery>) -> ApiResult<Json<Value>> {
    let s = st.session(&id).await?;
    let h = s.history().await?;
    let limit = q.limit.unwrap_or(200).clamp(1, 2000);
    Ok(Json(json!({
        "version": format!("{:016x}", h.fingerprint),
        "total": h.commits.len(),
        "width": h.layout.width,
        "offset": q.offset,
        "rows": graph_rows(&h, q.offset, limit),
    })))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

async fn search_graph(State(st): State<Shared>, Path(id): Path<String>, Query(q): Query<SearchQuery>) -> ApiResult<Json<Value>> {
    let h = st.session(&id).await?.history().await?;
    let rows = tokio::task::spawn_blocking(move || h.search(&q.q)).await.map_err(|e| anyhow::anyhow!("task failed: {e}"))?;
    Ok(Json(json!({ "rows": rows })))
}

#[derive(Deserialize)]
struct LocateQuery {
    /// Full or abbreviated commit ids, comma-separated.
    oids: String,
}

/// Row numbers of commits in the current graph; `null` for a commit that is not shown.
async fn locate(State(st): State<Shared>, Path(id): Path<String>, Query(q): Query<LocateQuery>) -> ApiResult<Json<Value>> {
    let h = st.session(&id).await?.history().await?;
    let rows: Vec<Option<u32>> =
        q.oids.split(',').map(|s| git2::Oid::from_str(s.trim()).ok().and_then(|o| h.index.get(&o).copied())).collect();
    Ok(Json(json!({ "version": format!("{:016x}", h.fingerprint), "rows": rows })))
}

async fn get_commit(State(st): State<Shared>, Path((id, oid)): Path<(String, String)>) -> ApiResult<Json<repo::CommitDetail>> {
    Ok(Json(st.with_repo(&id, move |r| repo::commit_detail(r, repo::parse_oid(&oid)?)).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiffQuery {
    /// A commit id: the diff against its first parent.
    commit: Option<String>,
    /// `staged` or `unstaged`: a diff of the working tree. Used when `commit` is absent.
    side: Option<worktree::Side>,
    path: String,
    old_path: Option<String>,
}

async fn get_diff(State(st): State<Shared>, Path(id): Path<String>, Query(q): Query<DiffQuery>) -> ApiResult<Json<repo::FileDiff>> {
    Ok(Json(
        st.with_repo(&id, move |r| match (&q.commit, q.side) {
            (Some(commit), _) => repo::commit_file_diff(r, repo::parse_oid(commit)?, &q.path, q.old_path.as_deref()),
            (None, Some(side)) => worktree::file_diff(r, side, &q.path, q.old_path.as_deref()),
            (None, None) => Err(anyhow::anyhow!("Give a commit or a side")),
        })
        .await?,
    ))
}

// ---------- handlers: working tree ----------

async fn get_status(State(st): State<Shared>, Path(id): Path<String>) -> ApiResult<Json<worktree::WorkStatus>> {
    Ok(Json(st.with_repo(&id, worktree::status).await?))
}

#[derive(Deserialize)]
struct PathsBody {
    paths: Vec<String>,
}

/// Runs a change of the repository, then refreshes the session so the UI hears about it
/// at once instead of waiting for the file watch.
async fn mutate<T: Send + 'static>(
    st: &Shared,
    id: &str,
    f: impl FnOnce(&git2::Repository, &Session) -> anyhow::Result<T> + Send + 'static,
) -> ApiResult<T> {
    let s = st.session(id).await?;
    let s2 = s.clone();
    let result =
        tokio::task::spawn_blocking(move || f(&repo::open(&s2.root)?, &s2)).await.map_err(|e| anyhow::anyhow!("task failed: {e}"))?;
    // Refresh also after a failure: a git command may have changed something before it failed.
    s.changed().await;
    Ok(result?)
}

async fn stage(State(st): State<Shared>, Path(id): Path<String>, Json(req): Json<PathsBody>) -> ApiResult<Json<Value>> {
    mutate(&st, &id, move |r, _| worktree::stage(r, &req.paths)).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn unstage(State(st): State<Shared>, Path(id): Path<String>, Json(req): Json<PathsBody>) -> ApiResult<Json<Value>> {
    mutate(&st, &id, move |r, _| worktree::unstage(r, &req.paths)).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn discard(State(st): State<Shared>, Path(id): Path<String>, Json(req): Json<PathsBody>) -> ApiResult<Json<Value>> {
    let snapshot = mutate(&st, &id, move |r, _| worktree::discard(r, &req.paths)).await?;
    Ok(Json(json!({ "snapshot": snapshot })))
}

#[derive(Deserialize)]
struct ApplyBody {
    op: worktree::PatchOp,
    path: String,
    hunks: Vec<worktree::HunkSelection>,
}

async fn apply_selection(State(st): State<Shared>, Path(id): Path<String>, Json(req): Json<ApplyBody>) -> ApiResult<Json<Value>> {
    let snapshot = mutate(&st, &id, move |r, _| worktree::apply_selection(r, req.op, &req.path, &req.hunks)).await?;
    Ok(Json(json!({ "snapshot": snapshot })))
}

async fn commit(
    State(st): State<Shared>,
    Path(id): Path<String>,
    Json(req): Json<worktree::CommitRequest>,
) -> ApiResult<Json<worktree::CommitResult>> {
    let has_git = st.git.is_some();
    Ok(Json(mutate(&st, &id, move |r, s| worktree::commit(r, &s.root, has_git, &req)).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetSoftBody {
    to: String,
    expect_head: String,
}

async fn reset_soft(State(st): State<Shared>, Path(id): Path<String>, Json(req): Json<ResetSoftBody>) -> ApiResult<Json<Value>> {
    mutate(&st, &id, move |r, _| worktree::reset_soft(r, &req.to, &req.expect_head)).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn list_snapshots(State(st): State<Shared>, Path(id): Path<String>) -> ApiResult<Json<Vec<safety::Snapshot>>> {
    Ok(Json(st.with_repo(&id, safety::list).await?))
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

async fn restore_snapshot(State(st): State<Shared>, Path(id): Path<String>, Json(req): Json<NameBody>) -> ApiResult<Json<Value>> {
    let files = mutate(&st, &id, move |r, _| safety::restore(r, &req.name)).await?;
    Ok(Json(json!({ "files": files })))
}

// ---------- handlers: operations and jobs ----------

async fn run_op(State(st): State<Shared>, Path(id): Path<String>, Json(op): Json<ops::Op>) -> ApiResult<Json<Value>> {
    Ok(Json(mutate(&st, &id, move |r, s| ops::run(r, &s.root, op)).await?))
}

async fn run_history(State(st): State<Shared>, Path(id): Path<String>, Json(op): Json<rewrite::HistoryOp>) -> ApiResult<Json<Value>> {
    if st.git.is_none() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "Git was not found on PATH. Install Git (https://git-scm.com/downloads) and restart the app.",
        ));
    }
    Ok(Json(mutate(&st, &id, move |r, s| rewrite::run(r, &s.root, op)).await?))
}

#[derive(Deserialize)]
struct PlanQuery {
    /// The commit below the range; the whole history when absent.
    base: Option<String>,
}

async fn rebase_plan(
    State(st): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<PlanQuery>,
) -> ApiResult<Json<Vec<rewrite::PlanCommit>>> {
    Ok(Json(st.with_repo(&id, move |r| rewrite::rebase_plan(r, q.base.as_deref())).await?))
}

#[derive(Deserialize)]
struct RebaseInfoQuery {
    branch: Option<String>,
    onto: String,
}

async fn rebase_info(State(st): State<Shared>, Path(id): Path<String>, Query(q): Query<RebaseInfoQuery>) -> ApiResult<Json<Value>> {
    Ok(Json(st.with_repo(&id, move |r| rewrite::rebase_info(r, q.branch.as_deref(), &q.onto)).await?))
}

async fn list_conflicts(State(st): State<Shared>, Path(id): Path<String>) -> ApiResult<Json<Vec<conflicts::ConflictFile>>> {
    Ok(Json(st.with_repo(&id, conflicts::list).await?))
}

#[derive(Deserialize)]
struct PathQuery {
    path: String,
}

async fn get_conflict(
    State(st): State<Shared>,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> ApiResult<Json<conflicts::ConflictDetail>> {
    Ok(Json(st.with_repo(&id, move |r| conflicts::detail(r, &q.path)).await?))
}

#[derive(Deserialize)]
struct ResolveBody {
    path: String,
    #[serde(flatten)]
    choice: conflicts::Choice,
}

async fn resolve_conflict(State(st): State<Shared>, Path(id): Path<String>, Json(req): Json<ResolveBody>) -> ApiResult<Json<Value>> {
    let snapshot = mutate(&st, &id, move |r, _| conflicts::resolve(r, &req.path, req.choice)).await?;
    Ok(Json(json!({ "snapshot": snapshot })))
}

/// Starts a network operation. The answer comes at once; progress and the result arrive
/// as `job` events.
async fn start_job(State(st): State<Shared>, Path(id): Path<String>, Json(job): Json<ops::NetJob>) -> ApiResult<Json<jobs::JobInfo>> {
    let s = st.session(&id).await?;
    let (title, args) = job.command();
    let rt = tokio::runtime::Handle::current();
    let session = s.clone();
    let info = st.jobs.start(Some(id), &title, s.root.clone(), args, move |_ok| {
        // Refresh in both cases: a failed pull can still have fetched new commits.
        rt.spawn(async move { session.changed().await });
    })?;
    Ok(Json(info))
}

#[derive(Deserialize)]
struct CloneBody {
    url: String,
    /// The folder that receives the new repository folder.
    parent: String,
    /// Name of the new folder; taken from the URL when absent.
    name: Option<String>,
}

async fn clone_repo(State(st): State<Shared>, Json(req): Json<CloneBody>) -> ApiResult<Json<Value>> {
    let url = req.url.trim().to_string();
    if url.is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "Enter the URL of the repository"));
    }
    let parent = PathBuf::from(req.parent.trim());
    if !parent.is_dir() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, format!("{} is not a folder", parent.display())));
    }
    let name =
        req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| ops::clone_dir_name(&url));
    if name.contains(['/', '\\']) || name == "." || name == ".." {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, format!("'{name}' is not a folder name")));
    }
    let target = parent.join(&name);
    if target.exists() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, format!("{} already exists", target.display())));
    }
    let args = vec!["clone".to_string(), "--progress".to_string(), url, name];
    let info = st.jobs.start(None, "Clone", parent, args, |_| {})?;
    Ok(Json(json!({ "job": info, "path": target.display().to_string() })))
}

async fn list_jobs(State(st): State<Shared>) -> Json<Vec<jobs::JobInfo>> {
    Json(st.jobs.list())
}

async fn cancel_job(State(st): State<Shared>, Path(job): Path<String>) -> Json<Value> {
    Json(json!({ "cancelled": st.jobs.cancel(&job) }))
}

async fn drop_snapshot(State(st): State<Shared>, Path(id): Path<String>, Json(req): Json<NameBody>) -> ApiResult<Json<Value>> {
    st.with_repo(&id, move |r| safety::drop(r, &req.name)).await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------- auth ----------

/// Desktop mode: the window opens `/?token=…` once; the server answers with an HttpOnly
/// cookie, and every later request must carry it. Other local processes get 403.
async fn app_token_guard(State(st): State<Shared>, headers: HeaderMap, req: Request, next: Next) -> Response {
    let Some(token) = &st.app_token else { return next.run(req).await };
    let cookie = format!("{APP_ID}_token=");
    let from_query = req.uri().query().is_some_and(|q| q.split('&').any(|kv| kv.strip_prefix("token=") == Some(token.as_str())));
    let from_cookie = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .any(|c| c.trim().strip_prefix(cookie.as_str()) == Some(token.as_str()));
    if from_query {
        let mut res = next.run(req).await;
        if let Ok(v) = HeaderValue::from_str(&format!("{cookie}{token}; HttpOnly; SameSite=Strict; Path=/")) {
            res.headers_mut().append(header::SET_COOKIE, v);
        }
        return res;
    }
    if from_cookie {
        return next.run(req).await;
    }
    (StatusCode::FORBIDDEN, format!("{APP_NAME}: this server belongs to a desktop window")).into_response()
}

/// Builds the HTTP application and its shared state.
pub fn router(opts: Options) -> (Router, Shared) {
    let (events, _) = broadcast::channel(256);
    let recent: Vec<Recent> =
        std::fs::read_to_string(opts.data_dir.join("recent.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let state: Shared = Arc::new(AppState {
        prefs: RwLock::new(prefs::load(&prefs::store_path(&opts.data_dir))),
        data_dir: opts.data_dir,
        app_token: opts.app_token,
        sessions: RwLock::new(HashMap::new()),
        recent: RwLock::new(recent),
        jobs: jobs::Jobs::new(events.clone()),
        events,
        git: gitcli::detect(),
        startup: RwLock::new(opts.open.iter().map(|p| p.display().to_string()).collect()),
    });

    let api = Router::new()
        .route("/info", get(info))
        .route("/events", get(event_stream))
        .route("/prefs", get(get_prefs).put(put_prefs))
        .route("/recent", get(list_recent))
        .route("/recent/forget", post(forget_recent))
        .route("/dialog/folder", post(pick_folder))
        .route("/repos/open", post(open_repo))
        .route("/repos/{id}", axum::routing::delete(close_repo))
        .route("/repos/{id}/refs", get(get_refs))
        .route("/repos/{id}/graph", get(get_graph))
        .route("/repos/{id}/graph/search", get(search_graph))
        .route("/repos/{id}/graph/locate", get(locate))
        .route("/repos/{id}/commit/{oid}", get(get_commit))
        .route("/repos/{id}/diff", get(get_diff))
        .route("/repos/{id}/status", get(get_status))
        .route("/repos/{id}/stage", post(stage))
        .route("/repos/{id}/unstage", post(unstage))
        .route("/repos/{id}/discard", post(discard))
        .route("/repos/{id}/apply", post(apply_selection))
        .route("/repos/{id}/commit", post(commit))
        .route("/repos/{id}/reset-soft", post(reset_soft))
        .route("/repos/{id}/snapshots", get(list_snapshots))
        .route("/repos/{id}/snapshots/restore", post(restore_snapshot))
        .route("/repos/{id}/snapshots/drop", post(drop_snapshot))
        .route("/repos/{id}/op", post(run_op))
        .route("/repos/{id}/history", post(run_history))
        .route("/repos/{id}/rebase-plan", get(rebase_plan))
        .route("/repos/{id}/rebase-info", get(rebase_info))
        .route("/repos/{id}/conflicts", get(list_conflicts))
        .route("/repos/{id}/conflict", get(get_conflict))
        .route("/repos/{id}/resolve", post(resolve_conflict))
        .route("/repos/{id}/job", post(start_job))
        .route("/clone", post(clone_repo))
        .route("/jobs", get(list_jobs))
        .route("/jobs/{job}", axum::routing::delete(cancel_job));

    let app = Router::new()
        .route("/", get(index_page))
        .nest("/api", api)
        .route("/{*path}", get(asset))
        .layer(middleware::from_fn_with_state(state.clone(), app_token_guard))
        // A panic in one request returns a JSON error instead of dropping the connection.
        .layer(tower_http::catch_panic::CatchPanicLayer::custom(|err: Box<dyn std::any::Any + Send>| {
            let msg = err
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown panic".into());
            tracing::error!("request panicked: {msg}");
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("Internal error: {msg}")).into_response()
        }))
        .with_state(state.clone());
    (app, state)
}
