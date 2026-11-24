//! Operations that change history: cherry-pick, revert, reset, interactive rebase, and
//! continuing or aborting an operation that stopped for conflicts.
//!
//! All of them run the system `git`. Every answer carries `before`, the commit the branch
//! pointed to, so the UI can offer to undo the operation.

use crate::ops::{in_progress, resolve};
use crate::repo::parse_oid;
use crate::{conflicts, gitcli, safety, worktree};
use anyhow::{anyhow, bail, Context, Result};
use git2::{Oid, Repository, RepositoryState, Sort};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum HistoryOp {
    /// Applies commits on top of HEAD, oldest first.
    CherryPick { oids: Vec<String> },
    /// Adds a commit that undoes `oid`.
    Revert { oid: String },
    /// Moves the current branch to `to`. `mode`: `soft` keeps index and files, `mixed` keeps
    /// files, `hard` makes index and files equal to the commit.
    Reset { to: String, mode: String },
    /// Rewrites the commits after `base` (all commits when absent) as `steps` describe.
    RebaseInteractive { base: Option<String>, steps: Vec<Step> },
    /// Goes on with the operation in progress after the conflicts are resolved.
    Continue,
    /// Skips the commit that stopped a rebase, cherry-pick or revert.
    Skip,
    /// Stops the operation in progress and returns to the state before it.
    Abort,
    /// Brings the conflict markers of a resolved file back.
    Unresolve { path: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct Step {
    pub oid: String,
    /// `pick`, `reword`, `squash` (meld into the commit before, keep both messages unless
    /// `message` of the group's first commit replaces them), `fixup` (meld, drop this
    /// message) or `drop`.
    pub action: String,
    /// The new message, for `reword` and for the first commit of a squashed group.
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlanCommit {
    pub oid: String,
    pub summary: String,
    pub message: String,
    pub author: String,
    pub time: i64,
    /// The commit is already on the upstream branch: rewriting it changes published history.
    pub published: bool,
    /// A merge commit. An interactive rebase would flatten it, so the UI refuses.
    pub merge: bool,
}

fn head_oid(repo: &Repository) -> Result<Oid> {
    Ok(repo.head().context("The repository has no commit yet")?.peel_to_commit()?.id())
}

/// The upstream commit of the current branch, if it has one.
fn upstream_tip(repo: &Repository) -> Option<Oid> {
    let head = repo.head().ok()?;
    let name = head.shorthand().ok()?;
    repo.find_branch(name, git2::BranchType::Local).ok()?.upstream().ok()?.get().target()
}

/// The commits an interactive rebase onto `base` would rewrite, oldest first.
pub fn rebase_plan(repo: &Repository, base: Option<&str>) -> Result<Vec<PlanCommit>> {
    let head = head_oid(repo)?;
    let base_oid = match base {
        Some(b) => Some(resolve(repo, Some(b))?.id()),
        None => None,
    };
    if let Some(b) = base_oid {
        if b != head && !repo.graph_descendant_of(head, b)? {
            bail!("The chosen commit is not an ancestor of the current branch");
        }
    }
    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::REVERSE)?;
    walk.push(head)?;
    if let Some(b) = base_oid {
        walk.hide(b)?;
    }
    let published: HashSet<Oid> = match upstream_tip(repo) {
        Some(up) => {
            let mut w = repo.revwalk()?;
            w.push(up)?;
            if let Some(b) = base_oid {
                w.hide(b)?;
            }
            w.filter_map(|o| o.ok()).collect()
        }
        None => HashSet::new(),
    };
    let mut out = Vec::new();
    for oid in walk {
        let oid = oid?;
        let c = repo.find_commit(oid)?;
        out.push(PlanCommit {
            oid: oid.to_string(),
            summary: c.summary().ok().flatten().unwrap_or("").to_string(),
            message: String::from_utf8_lossy(c.message_bytes()).trim_end().to_string(),
            author: String::from_utf8_lossy(c.author().name_bytes()).into_owned(),
            time: c.time().seconds(),
            published: published.contains(&oid),
            merge: c.parent_count() > 1,
        });
    }
    Ok(out)
}

/// Commits of `branch` that a rebase onto `onto` would rewrite, and how many of them are
/// already on the branch's upstream.
pub fn rebase_info(repo: &Repository, branch: Option<&str>, onto: &str) -> Result<Value> {
    let tip = resolve(repo, branch)?.id();
    let onto_oid = resolve(repo, Some(onto))?.id();
    let mut walk = repo.revwalk()?;
    walk.push(tip)?;
    walk.hide(onto_oid)?;
    let commits: Vec<Oid> = walk.filter_map(|o| o.ok()).collect();
    let upstream = match branch {
        Some(b) => repo.find_branch(b, git2::BranchType::Local).ok().and_then(|b| b.upstream().ok()).and_then(|u| u.get().target()),
        None => upstream_tip(repo),
    };
    let published = match upstream {
        Some(up) => commits.iter().filter(|c| **c == up || repo.graph_descendant_of(up, **c).unwrap_or(false)).count(),
        None => 0,
    };
    Ok(json!({ "commits": commits.len(), "published": published }))
}

/// Saves every changed file of the working tree before an operation overwrites them.
fn snapshot_dirty(repo: &Repository, reason: &str) -> Result<Option<safety::Snapshot>> {
    let status = worktree::status(repo)?;
    let mut paths: Vec<String> = status.unstaged.iter().chain(status.staged.iter()).map(|f| f.path.clone()).collect();
    paths.sort();
    paths.dedup();
    if paths.is_empty() {
        return Ok(None);
    }
    safety::snapshot(repo, &paths, reason)
}

fn merge_parent_args(repo: &Repository, oid: Oid) -> Result<Vec<&'static str>> {
    // A merge commit needs to know which parent is the mainline; the first parent is the
    // branch the merge was made on.
    Ok(if repo.find_commit(oid)?.parent_count() > 1 { vec!["-m", "1"] } else { Vec::new() })
}

fn interactive(repo: &Repository, root: &Path, base: Option<String>, steps: Vec<Step>) -> Result<Value> {
    let before = head_oid(repo)?.to_string();
    if worktree::is_dirty(repo) && !worktree::status(repo)?.staged.is_empty() {
        bail!("Commit or stash the staged changes before an interactive rebase");
    }
    let plan = rebase_plan(repo, base.as_deref())?;
    if plan.iter().any(|c| c.merge) {
        bail!("The range contains merge commits. An interactive rebase would turn them into ordinary commits, so it is not started.");
    }
    // The steps must name exactly the commits of the range, each once.
    let expected: HashSet<&str> = plan.iter().map(|c| c.oid.as_str()).collect();
    let given: HashSet<&str> = steps.iter().map(|s| s.oid.as_str()).collect();
    if expected != given || given.len() != steps.len() {
        bail!("The branch changed since the list was loaded. Reload it and try again.");
    }
    let work = repo.path().join("graftline");
    std::fs::create_dir_all(&work)?;
    let mut todo = String::new();
    // The message that replaces the message of the commit group being built.
    let mut pending: Option<String> = None;
    let mut group_open = false;
    let mut file_no = 0;
    let mut flush = |todo: &mut String, pending: &mut Option<String>| -> Result<()> {
        if let Some(msg) = pending.take() {
            file_no += 1;
            let file = work.join(format!("message-{file_no}.txt"));
            std::fs::write(&file, msg.trim_end().to_string() + "\n")?;
            todo.push_str(&format!("exec git commit --amend --cleanup=strip -F {}\n", gitcli::shell_quote(&file)));
        }
        Ok(())
    };
    for step in &steps {
        let msg = step.message.as_deref().map(str::trim).filter(|m| !m.is_empty());
        match step.action.as_str() {
            "pick" | "reword" => {
                flush(&mut todo, &mut pending)?;
                todo.push_str(&format!("pick {}\n", step.oid));
                group_open = true;
                if step.action == "reword" || msg.is_some() {
                    pending = Some(msg.ok_or_else(|| anyhow!("Enter the new message for {}", &step.oid[..7]))?.to_string());
                }
            }
            "squash" | "fixup" => {
                if !group_open {
                    bail!("The first commit cannot be squashed: there is no commit before it to meld into");
                }
                // With a replacement message for the group, the messages of the melded
                // commits are not needed: fixup drops them.
                let keep_messages = step.action == "squash" && pending.is_none();
                todo.push_str(&format!("{} {}\n", if keep_messages { "squash" } else { "fixup" }, step.oid));
            }
            "drop" => {}
            other => bail!("'{other}' is not a rebase action"),
        }
    }
    flush(&mut todo, &mut pending)?;
    if todo.is_empty() {
        bail!("Every commit is dropped. To remove all of them, reset the branch instead.");
    }
    let todo_file = work.join("rebase-todo");
    std::fs::write(&todo_file, todo)?;
    let exe = gitcli::editor_exe().context("Cannot find the program to prepare the rebase")?;
    let editor = format!("{} {} {}", gitcli::shell_quote(&exe), gitcli::APPLY_TODO, gitcli::shell_quote(&todo_file));
    let mut args = vec!["rebase", "-i"];
    match &base {
        Some(b) => args.push(b),
        None => args.push("--root"),
    }
    in_progress(root, gitcli::run_env(root, &args, None, &[("GIT_SEQUENCE_EDITOR", &editor)]), &before)
}

pub fn run(repo: &Repository, root: &Path, op: HistoryOp) -> Result<Value> {
    match op {
        HistoryOp::CherryPick { oids } => {
            if oids.is_empty() {
                bail!("Choose at least one commit");
            }
            let before = head_oid(repo)?.to_string();
            let mut args = vec!["cherry-pick".to_string()];
            if oids.len() == 1 {
                args.extend(merge_parent_args(repo, parse_oid(&oids[0])?)?.into_iter().map(str::to_string));
            }
            args.extend(oids.iter().cloned());
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            in_progress(root, gitcli::run(root, &refs, None), &before)
        }
        HistoryOp::Revert { oid } => {
            let before = head_oid(repo)?.to_string();
            let mut args = vec!["revert", "--no-edit"];
            args.extend(merge_parent_args(repo, parse_oid(&oid)?)?);
            args.push(&oid);
            in_progress(root, gitcli::run(root, &args, None), &before)
        }
        HistoryOp::Reset { to, mode } => {
            let before = head_oid(repo)?.to_string();
            let target = resolve(repo, Some(&to))?;
            let snapshot = match mode.as_str() {
                "soft" => {
                    repo.reset(target.as_object(), git2::ResetType::Soft, None)?;
                    crate::cmdlog::note(repo, &["reset", "--soft", &target.id().to_string()]);
                    None
                }
                "mixed" => {
                    repo.reset(target.as_object(), git2::ResetType::Mixed, None)?;
                    crate::cmdlog::note(repo, &["reset", "--mixed", &target.id().to_string()]);
                    None
                }
                "hard" => {
                    // The only reset that overwrites files: save them first.
                    let snapshot = snapshot_dirty(repo, "Before reset --hard")?;
                    gitcli::run(root, &["reset", "--hard", &target.id().to_string()], None)?;
                    snapshot
                }
                other => bail!("'{other}' is not a reset mode"),
            };
            Ok(json!({ "before": before, "snapshot": snapshot }))
        }
        HistoryOp::RebaseInteractive { base, steps } => interactive(repo, root, base, steps),
        HistoryOp::Continue => {
            conflicts::ensure_resolved(repo)?;
            let before = head_oid(repo).map(|o| o.to_string()).unwrap_or_default();
            let args: &[&str] = match repo.state() {
                RepositoryState::Merge => &["commit", "--no-edit"],
                RepositoryState::Rebase | RepositoryState::RebaseInteractive | RepositoryState::RebaseMerge => &["rebase", "--continue"],
                RepositoryState::CherryPick | RepositoryState::CherryPickSequence => &["cherry-pick", "--continue"],
                RepositoryState::Revert | RepositoryState::RevertSequence => &["revert", "--continue"],
                _ => bail!("No operation is in progress"),
            };
            in_progress(root, gitcli::run(root, args, None), &before)
        }
        HistoryOp::Skip => {
            let before = head_oid(repo).map(|o| o.to_string()).unwrap_or_default();
            let args: &[&str] = match repo.state() {
                RepositoryState::Rebase | RepositoryState::RebaseInteractive | RepositoryState::RebaseMerge => &["rebase", "--skip"],
                RepositoryState::CherryPick | RepositoryState::CherryPickSequence => &["cherry-pick", "--skip"],
                RepositoryState::Revert | RepositoryState::RevertSequence => &["revert", "--skip"],
                _ => bail!("There is no commit to skip"),
            };
            in_progress(root, gitcli::run(root, args, None), &before)
        }
        HistoryOp::Abort => {
            let args: &[&str] = match repo.state() {
                RepositoryState::Merge => &["merge", "--abort"],
                RepositoryState::Rebase | RepositoryState::RebaseInteractive | RepositoryState::RebaseMerge => &["rebase", "--abort"],
                RepositoryState::CherryPick | RepositoryState::CherryPickSequence => &["cherry-pick", "--abort"],
                RepositoryState::Revert | RepositoryState::RevertSequence => &["revert", "--abort"],
                _ => bail!("No operation is in progress"),
            };
            // Conflict resolutions made so far are lost by an abort: keep a copy.
            let snapshot = snapshot_dirty(repo, "Before abort")?;
            gitcli::run(root, args, None)?;
            Ok(json!({ "snapshot": snapshot }))
        }
        HistoryOp::Unresolve { path } => {
            let snapshot = safety::snapshot(repo, std::slice::from_ref(&path), "Before restoring conflict markers")?;
            gitcli::run(root, &["checkout", "--merge", "--", &path], None)?;
            Ok(json!({ "snapshot": snapshot }))
        }
    }
}
