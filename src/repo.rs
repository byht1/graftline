//! Reading a repository in process with libgit2: refs, commit details, diffs.
//!
//! Every function takes an open `Repository`. The server opens one per request on a blocking
//! thread; opening is cheap and avoids sharing a handle that is not thread-safe.

use crate::history;
use anyhow::{anyhow, Context, Result};
use git2::{Delta, Diff, DiffFindOptions, DiffOptions, Oid, Patch, Repository, RepositoryState};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Lines of one file diff sent to the UI at most. Larger diffs are cut and marked.
pub const MAX_DIFF_LINES: usize = 20_000;
/// Per-file line counts are computed for this many files of a commit.
const MAX_STAT_FILES: usize = 400;

/// Finds the repository that contains `path` and returns its root: the working directory,
/// or the git directory of a bare repository.
pub fn discover(path: &Path) -> Result<PathBuf> {
    let repo = Repository::discover(path).with_context(|| format!("{} is not inside a Git repository", path.display()))?;
    Ok(clean_path(repo.workdir().unwrap_or_else(|| repo.path())))
}

/// Absolute path without the Windows `\\?\` prefix and without a trailing separator.
pub fn clean_path(p: &Path) -> PathBuf {
    let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = abs.display().to_string();
    let s = s.strip_prefix(r"\\?\UNC\").map(|r| format!(r"\\{r}")).unwrap_or_else(|| s.strip_prefix(r"\\?\").unwrap_or(&s).to_string());
    PathBuf::from(s.trim_end_matches(['/', '\\']))
}

pub fn open(root: &Path) -> Result<Repository> {
    Repository::open(root).with_context(|| format!("Cannot open the repository at {}", root.display()))
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Head {
    /// Short name of the checked-out branch; `None` when HEAD is detached.
    pub branch: Option<String>,
    pub oid: Option<String>,
    pub detached: bool,
    /// The branch has no commits yet.
    pub unborn: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Branch {
    pub name: String,
    pub oid: String,
    pub current: bool,
    pub upstream: Option<String>,
    /// The configured upstream no longer exists.
    pub gone: bool,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RemoteBranch {
    /// Name without the remote prefix.
    pub name: String,
    /// `<remote>/<name>`.
    pub full: String,
    pub oid: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Remote {
    pub name: String,
    pub url: Option<String>,
    pub push_url: Option<String>,
    pub branches: Vec<RemoteBranch>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    pub name: String,
    /// The commit the tag points to.
    pub oid: String,
    pub annotated: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Stash {
    pub index: usize,
    pub oid: String,
    pub message: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Refs {
    pub head: Head,
    pub branches: Vec<Branch>,
    pub remotes: Vec<Remote>,
    pub tags: Vec<Tag>,
    pub stashes: Vec<Stash>,
    /// An operation in progress: `merge`, `rebase`, `cherry-pick`, `revert`, `bisect`; `None` when idle.
    pub state: Option<&'static str>,
    pub bare: bool,
}

pub fn state_name(state: RepositoryState) -> Option<&'static str> {
    match state {
        RepositoryState::Clean => None,
        RepositoryState::Merge => Some("merge"),
        RepositoryState::Revert | RepositoryState::RevertSequence => Some("revert"),
        RepositoryState::CherryPick | RepositoryState::CherryPickSequence => Some("cherry-pick"),
        RepositoryState::Bisect => Some("bisect"),
        RepositoryState::Rebase | RepositoryState::RebaseInteractive | RepositoryState::RebaseMerge => Some("rebase"),
        RepositoryState::ApplyMailbox | RepositoryState::ApplyMailboxOrRebase => Some("am"),
    }
}

pub fn head(repo: &Repository) -> Head {
    let symbolic = repo.find_reference("HEAD").ok().and_then(|h| h.symbolic_target().ok().flatten().map(str::to_string));
    let oid = repo.head().ok().and_then(|h| h.peel_to_commit().ok()).map(|c| c.id().to_string());
    let branch = symbolic.as_deref().and_then(|s| s.strip_prefix("refs/heads/")).map(str::to_string);
    Head { detached: symbolic.is_none(), unborn: oid.is_none(), branch, oid }
}

pub fn refs(repo: &Repository) -> Result<Refs> {
    let head = head(repo);
    let mut branches = Vec::new();
    for b in repo.branches(Some(git2::BranchType::Local))? {
        let Ok((b, _)) = b else { continue };
        let Some(name) = b.name().ok().flatten().map(str::to_string) else { continue };
        let Some(oid) = b.get().target() else { continue };
        let configured = repo.branch_upstream_name(&format!("refs/heads/{name}")).ok();
        let upstream_ref = b.upstream().ok();
        let upstream = upstream_ref.as_ref().and_then(|u| u.name().ok().flatten().map(str::to_string)).or_else(|| {
            configured.as_ref().and_then(|c| c.as_str().ok().and_then(|s| s.strip_prefix("refs/remotes/")).map(str::to_string))
        });
        let (ahead, behind) =
            upstream_ref.as_ref().and_then(|u| u.get().target()).and_then(|up| repo.graph_ahead_behind(oid, up).ok()).unwrap_or((0, 0));
        branches.push(Branch {
            current: head.branch.as_deref() == Some(name.as_str()),
            gone: upstream.is_some() && upstream_ref.is_none(),
            name,
            oid: oid.to_string(),
            upstream,
            ahead,
            behind,
        });
    }
    branches.sort_by_key(|b| b.name.to_lowercase());

    let mut remotes: Vec<Remote> = Vec::new();
    if let Ok(names) = repo.remotes() {
        for name in names.iter().flatten().flatten() {
            let r = repo.find_remote(name).ok();
            remotes.push(Remote {
                name: name.to_string(),
                url: r.as_ref().and_then(|r| r.url().ok().map(str::to_string)),
                push_url: r.as_ref().and_then(|r| r.pushurl().ok().flatten().map(str::to_string)),
                branches: Vec::new(),
            });
        }
    }
    // Longest remote name first, so "origin/team" wins over "origin" for "origin/team/x".
    let mut order: Vec<usize> = (0..remotes.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(remotes[i].name.len()));
    for b in repo.branches(Some(git2::BranchType::Remote))? {
        let Ok((b, _)) = b else { continue };
        let Some(full) = b.name().ok().flatten().map(str::to_string) else { continue };
        if full.ends_with("/HEAD") {
            continue;
        }
        let Ok(commit) = b.get().peel_to_commit() else { continue };
        let owner = order.iter().copied().find(|&i| full.strip_prefix(remotes[i].name.as_str()).is_some_and(|r| r.starts_with('/')));
        if let Some(i) = owner {
            let name = full[remotes[i].name.len() + 1..].to_string();
            remotes[i].branches.push(RemoteBranch { name, full, oid: commit.id().to_string() });
        }
    }
    for r in &mut remotes {
        r.branches.sort_by_key(|b| b.name.to_lowercase());
    }

    let mut tags = Vec::new();
    for r in repo.references_glob("refs/tags/*")? {
        let Ok(r) = r else { continue };
        let Some(name) = r.shorthand().ok().map(str::to_string) else { continue };
        let Ok(commit) = r.peel_to_commit() else { continue };
        let annotated = r.target().is_some_and(|t| t != commit.id());
        tags.push(Tag { name, oid: commit.id().to_string(), annotated });
    }
    tags.sort_by_key(|t| std::cmp::Reverse(natural_key(&t.name)));

    let stashes = history::stashes(repo)
        .into_iter()
        .enumerate()
        .map(|(index, (oid, message))| Stash { index, oid: oid.to_string(), message })
        .collect();

    Ok(Refs { head, branches, remotes, tags, stashes, state: state_name(repo.state()), bare: repo.is_bare() })
}

/// Sort key that orders "v1.10" after "v1.9".
fn natural_key(s: &str) -> Vec<(u8, u64, String)> {
    let mut key = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut n = 0u64;
            while let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) {
                n = n.saturating_mul(10).saturating_add(d as u64);
                chars.next();
            }
            key.push((1, n, String::new()));
        } else {
            let mut t = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() {
                    break;
                }
                t.push(c.to_ascii_lowercase());
                chars.next();
            }
            key.push((0, 0, t));
        }
    }
    key
}

// ---------------------------------------------------------------- commits

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    pub name: String,
    pub email: String,
    /// Seconds since the Unix epoch.
    pub time: i64,
    /// Time zone offset in minutes.
    pub offset: i32,
}

fn person(sig: &git2::Signature) -> Person {
    Person {
        name: String::from_utf8_lossy(sig.name_bytes()).into_owned(),
        email: String::from_utf8_lossy(sig.email_bytes()).into_owned(),
        time: sig.when().seconds(),
        offset: sig.when().offset_minutes(),
    }
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
    /// The earlier path of a renamed or copied file.
    pub old_path: Option<String>,
    /// `A` added, `M` modified, `D` deleted, `R` renamed, `C` copied, `T` type changed,
    /// `U` conflicted, `?` untracked.
    pub status: char,
    pub additions: Option<usize>,
    pub deletions: Option<usize>,
    pub binary: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CommitDetail {
    pub oid: String,
    pub parents: Vec<String>,
    pub author: Person,
    pub committer: Person,
    pub summary: String,
    pub body: String,
    pub files: Vec<FileChange>,
    pub additions: usize,
    pub deletions: usize,
}

pub fn parse_oid(s: &str) -> Result<Oid> {
    Oid::from_str(s).map_err(|_| anyhow!("'{s}' is not a commit id"))
}

fn path_string(p: Option<&Path>) -> String {
    p.map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default()
}

fn status_char(d: Delta) -> char {
    match d {
        Delta::Added => 'A',
        Delta::Deleted => 'D',
        Delta::Renamed => 'R',
        Delta::Copied => 'C',
        Delta::Typechange => 'T',
        Delta::Untracked => '?',
        Delta::Conflicted => 'U',
        _ => 'M',
    }
}

/// The diff a commit introduces against its `parent`-th parent (the empty tree for a root).
pub fn commit_diff<'r>(repo: &'r Repository, oid: Oid, parent: usize, path: Option<&[&str]>) -> Result<Diff<'r>> {
    let commit = repo.find_commit(oid)?;
    let new_tree = commit.tree()?;
    let old_tree = match commit.parent(parent) {
        Ok(p) => Some(p.tree()?),
        Err(_) => None,
    };
    let mut opts = DiffOptions::new();
    opts.context_lines(3);
    if let Some(paths) = path {
        opts.disable_pathspec_match(true);
        for p in paths {
            opts.pathspec(p);
        }
    }
    let mut diff = repo.diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), Some(&mut opts))?;
    diff.find_similar(Some(DiffFindOptions::new().renames(true)))?;
    Ok(diff)
}

/// Lists the files a diff touches. Line counts are computed for the first files only.
pub fn diff_files(diff: &Diff) -> Result<(Vec<FileChange>, usize, usize)> {
    let mut files = Vec::new();
    let (mut add_total, mut del_total) = (0, 0);
    for (i, delta) in diff.deltas().enumerate() {
        let new_path = path_string(delta.new_file().path());
        let old_path = path_string(delta.old_file().path());
        let status = status_char(delta.status());
        let path = if status == 'D' { old_path.clone() } else { new_path.clone() };
        let mut change = FileChange {
            old_path: (matches!(status, 'R' | 'C') && old_path != path).then_some(old_path),
            path,
            status,
            additions: None,
            deletions: None,
            binary: false,
        };
        if i < MAX_STAT_FILES {
            if let Ok(Some(patch)) = Patch::from_diff(diff, i) {
                change.binary = patch.delta().flags().is_binary();
                if let Ok((_, add, del)) = patch.line_stats() {
                    change.additions = Some(add);
                    change.deletions = Some(del);
                    add_total += add;
                    del_total += del;
                }
            }
        }
        files.push(change);
    }
    Ok((files, add_total, del_total))
}

pub fn commit_detail(repo: &Repository, oid: Oid) -> Result<CommitDetail> {
    let commit = repo.find_commit(oid).with_context(|| format!("Commit {oid} not found"))?;
    let message = String::from_utf8_lossy(commit.message_bytes()).into_owned();
    let (summary, body) = match message.split_once('\n') {
        Some((s, b)) => (s.trim_end().to_string(), b.trim_matches('\n').to_string()),
        None => (message.trim_end().to_string(), String::new()),
    };
    let diff = commit_diff(repo, oid, 0, None)?;
    let (files, additions, deletions) = diff_files(&diff)?;
    let (author, committer) = (person(&commit.author()), person(&commit.committer()));
    Ok(CommitDetail {
        oid: oid.to_string(),
        parents: commit.parent_ids().map(|p| p.to_string()).collect(),
        author,
        committer,
        summary,
        body,
        files,
        additions,
        deletions,
    })
}

// ---------------------------------------------------------------- file diff

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
    /// ` ` context, `+` added, `-` removed, `\` "no newline at end of file" for the line above.
    pub t: char,
    /// Line number in the old file.
    pub o: Option<u32>,
    /// Line number in the new file.
    pub n: Option<u32>,
    /// Text without the line ending.
    pub s: String,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Hunk {
    pub header: String,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub status: char,
    pub binary: bool,
    /// The diff has more lines than [`MAX_DIFF_LINES`]; the rest is not included.
    pub truncated: bool,
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    pub hunks: Vec<Hunk>,
}

fn mode_string(m: git2::FileMode) -> Option<String> {
    let v: u32 = m.into();
    (v != 0).then(|| format!("{v:06o}"))
}

/// Converts one delta of a diff into hunks and lines.
pub fn patch_to_file_diff(diff: &Diff, index: usize) -> Result<FileDiff> {
    let delta = diff.get_delta(index).ok_or_else(|| anyhow!("No such file in the diff"))?;
    let status = status_char(delta.status());
    let new_path = path_string(delta.new_file().path());
    let old_path = path_string(delta.old_file().path());
    let path = if status == 'D' { old_path.clone() } else { new_path };
    let mut out = FileDiff {
        old_path: (old_path != path && !old_path.is_empty()).then_some(old_path),
        path,
        status,
        old_mode: mode_string(delta.old_file().mode()),
        new_mode: mode_string(delta.new_file().mode()),
        ..Default::default()
    };
    let Some(patch) = Patch::from_diff(diff, index)? else {
        out.binary = true;
        return Ok(out);
    };
    out.binary = patch.delta().flags().is_binary();
    let mut total = 0usize;
    'hunks: for h in 0..patch.num_hunks() {
        let (hunk, count) = patch.hunk(h)?;
        let mut lines = Vec::with_capacity(count);
        for l in 0..count {
            if total >= MAX_DIFF_LINES {
                out.truncated = true;
                out.hunks.push(Hunk {
                    header: String::from_utf8_lossy(hunk.header()).trim_end().to_string(),
                    old_start: hunk.old_start(),
                    old_lines: hunk.old_lines(),
                    new_start: hunk.new_start(),
                    new_lines: hunk.new_lines(),
                    lines,
                });
                break 'hunks;
            }
            let line = patch.line_in_hunk(h, l)?;
            let t = match line.origin() {
                '+' | '-' | ' ' => line.origin(),
                '=' | '>' | '<' => '\\',
                _ => continue,
            };
            let text = String::from_utf8_lossy(line.content());
            let s = if t == '\\' { "No newline at end of file".to_string() } else { text.trim_end_matches(['\n', '\r']).to_string() };
            lines.push(DiffLine { t, o: line.old_lineno(), n: line.new_lineno(), s });
            total += 1;
        }
        out.hunks.push(Hunk {
            header: String::from_utf8_lossy(hunk.header()).trim_end().to_string(),
            old_start: hunk.old_start(),
            old_lines: hunk.old_lines(),
            new_start: hunk.new_start(),
            new_lines: hunk.new_lines(),
            lines,
        });
    }
    Ok(out)
}

/// Finds the delta for `path` (new or old path) and converts it.
pub fn file_diff(diff: &Diff, path: &str) -> Result<FileDiff> {
    let index = diff
        .deltas()
        .position(|d| path_string(d.new_file().path()) == path || path_string(d.old_file().path()) == path)
        .ok_or_else(|| anyhow!("{path} has no changes here"))?;
    patch_to_file_diff(diff, index)
}

/// Diff of one file in a commit against its first parent.
pub fn commit_file_diff(repo: &Repository, oid: Oid, path: &str, old_path: Option<&str>) -> Result<FileDiff> {
    let mut paths = vec![path];
    if let Some(old) = old_path {
        paths.push(old);
    }
    let diff = commit_diff(repo, oid, 0, Some(&paths))?;
    file_diff(&diff, path)
}
