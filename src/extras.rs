//! File history, blame, submodules and worktrees.

use crate::repo::parse_oid;
use anyhow::{anyhow, Context, Result};
use git2::{Oid, Repository, Sort};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileCommit {
    pub oid: String,
    pub summary: String,
    pub author: String,
    pub email: String,
    pub time: i64,
    /// `A` the commit added the file, `D` deleted it, `M` changed it.
    pub status: char,
}

/// Commits reachable from HEAD that changed `path`, newest first.
/// A commit counts when the file differs from the file in every parent, so a merge that
/// only brings in a change made on one side is not listed. Renames are not followed.
pub fn file_history(repo: &Repository, path: &str, limit: usize) -> Result<(Vec<FileCommit>, bool)> {
    let p = Path::new(path);
    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
    walk.push_head().context("The repository has no commit yet")?;
    let entry = |tree: &git2::Tree| tree.get_path(p).ok().map(|e| e.id());
    let mut out = Vec::new();
    let mut more = false;
    for oid in walk {
        let commit = repo.find_commit(oid?)?;
        let mine = entry(&commit.tree()?);
        let parents: Vec<Option<Oid>> = commit.parents().map(|c| c.tree().ok().and_then(|t| entry(&t))).collect();
        let changed = if parents.is_empty() { mine.is_some() } else { parents.iter().all(|theirs| *theirs != mine) };
        if !changed {
            continue;
        }
        if out.len() >= limit {
            more = true;
            break;
        }
        let was = parents.first().copied().flatten();
        let sig = commit.author();
        out.push(FileCommit {
            oid: commit.id().to_string(),
            summary: commit.summary().ok().flatten().unwrap_or("").to_string(),
            author: String::from_utf8_lossy(sig.name_bytes()).into_owned(),
            email: String::from_utf8_lossy(sig.email_bytes()).into_owned(),
            time: commit.time().seconds(),
            status: match (was.is_some(), mine.is_some()) {
                (false, true) => 'A',
                (true, false) => 'D',
                _ => 'M',
            },
        });
    }
    Ok((out, more))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlameHunk {
    /// First line of the hunk, 1-based.
    pub start: usize,
    pub lines: usize,
    pub oid: String,
    pub summary: String,
    pub author: String,
    pub time: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Blame {
    pub path: String,
    /// The commit whose version of the file is shown.
    pub commit: String,
    pub lines: Vec<String>,
    pub hunks: Vec<BlameHunk>,
}

/// Blame of `path` as of `commit` (HEAD when absent).
pub fn blame(repo: &Repository, path: &str, commit: Option<&str>) -> Result<Blame> {
    let at = match commit {
        Some(c) => repo.find_commit(parse_oid(c)?)?,
        None => repo.head().context("The repository has no commit yet")?.peel_to_commit()?,
    };
    let entry =
        at.tree()?.get_path(Path::new(path)).with_context(|| format!("{path} does not exist in commit {}", &at.id().to_string()[..7]))?;
    let blob = repo.find_blob(entry.id()).map_err(|_| anyhow!("{path} is not a file"))?;
    if blob.is_binary() {
        return Err(anyhow!("{path} is a binary file"));
    }
    let text = String::from_utf8_lossy(blob.content());
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut opts = git2::BlameOptions::new();
    opts.newest_commit(at.id());
    let blame = repo.blame_file(Path::new(path), Some(&mut opts)).with_context(|| format!("Cannot compute the blame of {path}"))?;
    let mut hunks = Vec::new();
    for h in blame.iter() {
        let oid = h.final_commit_id();
        let (summary, author, time) = match repo.find_commit(oid) {
            Ok(c) => (
                c.summary().ok().flatten().unwrap_or("").to_string(),
                String::from_utf8_lossy(c.author().name_bytes()).into_owned(),
                c.time().seconds(),
            ),
            Err(_) => (String::new(), String::new(), 0),
        };
        hunks.push(BlameHunk { start: h.final_start_line(), lines: h.lines_in_hunk(), oid: oid.to_string(), summary, author, time });
    }
    Ok(Blame { path: path.to_string(), commit: at.id().to_string(), lines, hunks })
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Submodule {
    pub name: String,
    pub path: String,
    pub url: Option<String>,
    /// The commit the parent repository records for the submodule.
    pub recorded: Option<String>,
    /// The commit checked out in the submodule's working tree.
    pub checked_out: Option<String>,
    /// The submodule is cloned and can be opened as a repository.
    pub initialized: bool,
    /// Absolute path, for opening it in a tab.
    pub full_path: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Worktree {
    pub name: String,
    pub path: String,
    pub branch: Option<String>,
    pub locked: bool,
    /// The worktree's folder no longer exists.
    pub missing: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Extras {
    pub submodules: Vec<Submodule>,
    pub worktrees: Vec<Worktree>,
}

pub fn extras(repo: &Repository) -> Result<Extras> {
    let workdir = repo.workdir().map(Path::to_path_buf);
    let mut submodules = Vec::new();
    for sm in repo.submodules().unwrap_or_default() {
        let path = sm.path().to_string_lossy().replace('\\', "/");
        let full = workdir.as_ref().map(|w| w.join(sm.path())).unwrap_or_default();
        submodules.push(Submodule {
            name: sm.name().ok().unwrap_or(&path).to_string(),
            url: sm.url().ok().flatten().map(str::to_string),
            recorded: sm.head_id().or_else(|| sm.index_id()).map(|o| o.to_string()),
            checked_out: sm.workdir_id().map(|o| o.to_string()),
            initialized: sm.open().is_ok(),
            full_path: crate::repo::clean_path(&full).display().to_string(),
            path,
        });
    }
    let mut worktrees = Vec::new();
    if let Ok(names) = repo.worktrees() {
        for name in names.iter().flatten().flatten() {
            let Ok(wt) = repo.find_worktree(name) else { continue };
            let path = wt.path().to_path_buf();
            let branch = Repository::open(&path).ok().and_then(|r| crate::repo::head(&r).branch);
            worktrees.push(Worktree {
                name: name.to_string(),
                missing: !path.exists(),
                path: crate::repo::clean_path(&path).display().to_string(),
                branch,
                locked: !matches!(wt.is_locked(), Ok(git2::WorktreeLockStatus::Unlocked)),
            });
        }
    }
    Ok(Extras { submodules, worktrees })
}
