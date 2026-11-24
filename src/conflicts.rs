//! Merge conflicts: the list of conflicted files, the three versions of one file, and
//! resolving a file by side or with edited content.

use crate::safety;
use anyhow::{anyhow, bail, Context, Result};
use git2::{IndexEntry, Repository, RepositoryState};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConflictFile {
    pub path: String,
    /// `both-modified`, `both-added`, `deleted-by-us`, `deleted-by-them` or `both-deleted`.
    pub kind: &'static str,
    /// The file exists on our side / their side / in the common ancestor.
    pub ours: bool,
    pub theirs: bool,
    pub base: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictDetail {
    pub path: String,
    pub kind: &'static str,
    /// The versions as text; `None` when that side does not have the file.
    pub base: Option<String>,
    pub ours: Option<String>,
    pub theirs: Option<String>,
    /// The file in the working tree, with conflict markers.
    pub merged: Option<String>,
    /// A side is binary or not valid UTF-8: only a whole side can be chosen.
    pub binary: bool,
    /// What the two sides are, for the labels in the UI.
    pub ours_label: String,
    pub theirs_label: String,
}

fn entry_path(e: &IndexEntry) -> String {
    String::from_utf8_lossy(&e.path).replace('\\', "/")
}

fn kind_of(ours: bool, theirs: bool, base: bool) -> &'static str {
    match (ours, theirs, base) {
        (true, true, true) => "both-modified",
        (true, true, false) => "both-added",
        (false, true, _) => "deleted-by-us",
        (true, false, _) => "deleted-by-them",
        (false, false, _) => "both-deleted",
    }
}

pub fn list(repo: &Repository) -> Result<Vec<ConflictFile>> {
    let index = repo.index()?;
    let mut out = Vec::new();
    for c in index.conflicts()? {
        let c = c?;
        let Some(path) = c.our.as_ref().or(c.their.as_ref()).or(c.ancestor.as_ref()).map(entry_path) else { continue };
        let (ours, theirs, base) = (c.our.is_some(), c.their.is_some(), c.ancestor.is_some());
        out.push(ConflictFile { path, kind: kind_of(ours, theirs, base), ours, theirs, base });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Names the two sides. In a merge "ours" is the current branch. In a rebase or cherry-pick
/// "ours" is the branch the commits are placed on and "theirs" is the commit being applied,
/// which is often the user's own work: the labels say so.
fn labels(repo: &Repository) -> (String, String) {
    let branch = crate::repo::head(repo).branch;
    let head = branch.clone().unwrap_or_else(|| "HEAD".into());
    let short = |file: &str| {
        std::fs::read_to_string(repo.path().join(file))
            .ok()
            .and_then(|s| s.split_whitespace().next().map(|o| o.chars().take(7).collect::<String>()))
    };
    match repo.state() {
        RepositoryState::Merge => {
            let theirs = short("MERGE_HEAD").map_or("the merged branch".to_string(), |o| format!("merged commit {o}"));
            (format!("{head} (current branch)"), theirs)
        }
        RepositoryState::Rebase | RepositoryState::RebaseInteractive | RepositoryState::RebaseMerge => {
            let theirs = short("REBASE_HEAD").map_or("the commit being rebased".to_string(), |o| format!("your commit {o} being rebased"));
            ("the new base".to_string(), theirs)
        }
        RepositoryState::CherryPick | RepositoryState::CherryPickSequence => {
            let theirs = short("CHERRY_PICK_HEAD").map_or("the picked commit".to_string(), |o| format!("picked commit {o}"));
            (format!("{head} (current branch)"), theirs)
        }
        RepositoryState::Revert | RepositoryState::RevertSequence => {
            (format!("{head} (current branch)"), "the reverted change".to_string())
        }
        _ => (head, "incoming".to_string()),
    }
}

fn find(repo: &Repository, path: &str) -> Result<git2::IndexConflict> {
    let index = repo.index()?;
    for c in index.conflicts()? {
        let c = c?;
        if c.our.as_ref().or(c.their.as_ref()).or(c.ancestor.as_ref()).map(entry_path).as_deref() == Some(path) {
            return Ok(c);
        }
    }
    Err(anyhow!("{path} has no conflict"))
}

pub fn detail(repo: &Repository, path: &str) -> Result<ConflictDetail> {
    let c = find(repo, path)?;
    let mut binary = false;
    let mut text = |e: &Option<IndexEntry>| -> Result<Option<String>> {
        let Some(e) = e else { return Ok(None) };
        let blob = repo.find_blob(e.id)?;
        match std::str::from_utf8(blob.content()) {
            Ok(s) if !blob.is_binary() => Ok(Some(s.to_string())),
            _ => {
                binary = true;
                Ok(Some(String::new()))
            }
        }
    };
    let (base, ours, theirs) = (text(&c.ancestor)?, text(&c.our)?, text(&c.their)?);
    let merged = repo.workdir().and_then(|w| std::fs::read(w.join(path)).ok()).and_then(|bytes| String::from_utf8(bytes).ok());
    let (ours_label, theirs_label) = labels(repo);
    Ok(ConflictDetail {
        path: path.to_string(),
        kind: kind_of(c.our.is_some(), c.their.is_some(), c.ancestor.is_some()),
        base,
        ours,
        theirs,
        merged,
        binary,
        ours_label,
        theirs_label,
    })
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "choice", rename_all = "kebab-case")]
pub enum Choice {
    /// Take our version of the whole file (or keep it deleted when we deleted it).
    Ours,
    /// Take their version of the whole file (or delete it when they deleted it).
    Theirs,
    /// Use this text as the resolved file.
    Content { content: String },
    /// Resolve by deleting the file.
    Delete,
}

/// Resolves the conflict of one file and stages the result. The working-tree file with the
/// conflict markers is saved in a snapshot first: it may hold a half-finished manual merge.
pub fn resolve(repo: &Repository, path: &str, choice: Choice) -> Result<Option<safety::Snapshot>> {
    let c = find(repo, path)?;
    let workdir = repo.workdir().ok_or_else(|| anyhow!("A bare repository has no working tree"))?;
    let abs = workdir.join(path);
    let snapshot = safety::snapshot(repo, &[path.to_string()], "Before resolving a conflict")?;
    let side = |entry: &Option<IndexEntry>, use_ours: bool| -> Result<bool> {
        if entry.is_none() {
            return Ok(false);
        }
        let mut co = git2::build::CheckoutBuilder::new();
        co.path(path).force().update_index(false).use_ours(use_ours).use_theirs(!use_ours);
        repo.checkout_index(None, Some(&mut co)).with_context(|| format!("Cannot write {path}"))?;
        Ok(true)
    };
    let equivalent: &[&str] = match &choice {
        Choice::Ours => &["checkout", "--ours", "--", path],
        Choice::Theirs => &["checkout", "--theirs", "--", path],
        Choice::Delete | Choice::Content { .. } => &[],
    };
    let keep = match choice {
        Choice::Ours => side(&c.our, true)?,
        Choice::Theirs => side(&c.their, false)?,
        Choice::Delete => false,
        Choice::Content { content } => {
            // A text area gives LF line endings. Keep CRLF when the file used it.
            let was_crlf = std::fs::read(&abs).map(|old| old.windows(2).any(|w| w == b"\r\n")).unwrap_or(false);
            let text = if was_crlf && !content.contains('\r') { content.replace('\n', "\r\n") } else { content };
            if let Some(dir) = abs.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&abs, text).with_context(|| format!("Cannot write {path}"))?;
            true
        }
    };
    let mut index = repo.index()?;
    if keep {
        index.add_path(Path::new(path)).with_context(|| format!("Cannot stage {path}"))?;
    } else {
        if std::fs::symlink_metadata(&abs).is_ok() {
            std::fs::remove_file(&abs).with_context(|| format!("Cannot delete {path}"))?;
        }
        index.remove_path(Path::new(path))?;
        // remove_path clears stage 0 only when the file was conflicted; drop what is left.
        index.conflict_remove(Path::new(path)).ok();
    }
    index.write()?;
    if !equivalent.is_empty() {
        crate::cmdlog::note(repo, equivalent);
    }
    // Staging the result is what marks the conflict as resolved.
    crate::cmdlog::note(repo, &[if keep { "add" } else { "rm" }, "--", path]);
    Ok(snapshot)
}

/// Checks that an operation may continue: no file is left in conflict.
pub fn ensure_resolved(repo: &Repository) -> Result<()> {
    let left = list(repo)?;
    if !left.is_empty() {
        bail!("Resolve the conflicts first: {}", left.iter().map(|c| c.path.as_str()).collect::<Vec<_>>().join(", "));
    }
    Ok(())
}
