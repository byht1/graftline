//! Operations on branches, tags, stashes and remotes.
//!
//! Rule: an operation that only moves or creates refs runs in process with libgit2 and
//! works without git installed. An operation that writes files in the working tree runs
//! the system `git`, so hooks, filters (Git LFS) and submodules behave as on the command line.

use crate::gitcli;
use crate::repo::parse_oid;
use anyhow::{anyhow, bail, Context, Result};
use git2::{BranchType, Oid, Repository};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

/// One operation, as the UI sends it.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum Op {
    BranchCreate {
        name: String,
        /// Commit id, branch or tag; HEAD when absent.
        at: Option<String>,
        #[serde(default)]
        checkout: bool,
    },
    BranchRename {
        name: String,
        to: String,
    },
    /// Without `force`, a branch with commits that exist nowhere else is not deleted; the
    /// answer lists them so the UI can ask.
    BranchDelete {
        name: String,
        #[serde(default)]
        force: bool,
    },
    /// Recreates a branch at a commit (undo of a delete).
    BranchRestore {
        name: String,
        oid: String,
    },
    SetUpstream {
        branch: String,
        /// `<remote>/<branch>`; `None` removes the upstream.
        upstream: Option<String>,
    },
    Checkout {
        /// Local branch name, `<remote>/<branch>`, tag name or commit id.
        target: String,
        /// `branch`, `remote`, `tag` or `commit`.
        kind: String,
    },
    TagCreate {
        name: String,
        at: Option<String>,
        /// A non-empty message makes an annotated tag.
        message: Option<String>,
    },
    TagDelete {
        name: String,
    },
    /// Points a tag at the object it had before (undo of a delete).
    TagRestore {
        name: String,
        oid: String,
    },
    /// Merges `branch` into `into` (the current branch when absent).
    Merge {
        branch: String,
        into: Option<String>,
    },
    /// Rebases `branch` (the current branch when absent) onto `onto`.
    Rebase {
        branch: Option<String>,
        onto: String,
    },
    StashSave {
        message: Option<String>,
        #[serde(default)]
        untracked: bool,
        #[serde(default)]
        keep_index: bool,
    },
    StashApply {
        index: usize,
    },
    StashPop {
        index: usize,
    },
    StashDrop {
        index: usize,
    },
    /// Puts a dropped stash back (undo of a drop).
    StashStore {
        oid: String,
        message: String,
    },
    RemoteAdd {
        name: String,
        url: String,
    },
    RemoteRemove {
        name: String,
    },
    RemoteRename {
        name: String,
        to: String,
    },
    RemoteSetUrl {
        name: String,
        url: String,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LostCommit {
    pub oid: String,
    pub summary: String,
}

/// Resolves a commit id, branch or tag name to a commit.
pub fn resolve<'r>(repo: &'r Repository, spec: Option<&str>) -> Result<git2::Commit<'r>> {
    match spec.map(str::trim).filter(|s| !s.is_empty()) {
        None => repo.head().context("The repository has no commit yet")?.peel_to_commit().map_err(Into::into),
        Some(s) => {
            repo.revparse_single(s).and_then(|o| o.peel_to_commit()).with_context(|| format!("'{s}' is not a commit, branch or tag"))
        }
    }
}

fn check_branch_name(name: &str) -> Result<()> {
    if !git2::Branch::name_is_valid(name).unwrap_or(false) || name == "HEAD" {
        bail!("'{name}' is not a valid branch name");
    }
    Ok(())
}

/// Commits that only `tip` reaches: not on another branch, remote branch or tag, and not
/// on HEAD. Deleting the ref of `skip_ref` would leave them reachable only through the reflog.
pub fn commits_only_on(repo: &Repository, tip: Oid, skip_ref: &str, limit: usize) -> Result<Vec<LostCommit>> {
    let mut walk = repo.revwalk()?;
    walk.push(tip)?;
    for r in repo.references()? {
        let Ok(r) = r else { continue };
        let Ok(name) = r.name() else { continue };
        let counts = name.starts_with("refs/heads/") || name.starts_with("refs/remotes/") || name.starts_with("refs/tags/");
        if !counts || name == skip_ref {
            continue;
        }
        if let Ok(c) = r.peel_to_commit() {
            walk.hide(c.id())?;
        }
    }
    // A detached HEAD keeps its commits visible too.
    if let Ok(head) = repo.head() {
        if head.name().ok() != Some(skip_ref) {
            if let Ok(c) = head.peel_to_commit() {
                walk.hide(c.id())?;
            }
        }
    }
    let mut out = Vec::new();
    for oid in walk.take(limit) {
        let oid = oid?;
        let summary = repo.find_commit(oid)?.summary().ok().flatten().unwrap_or("").to_string();
        out.push(LostCommit { oid: oid.to_string(), summary });
    }
    Ok(out)
}

fn stash_spec(index: usize) -> String {
    format!("stash@{{{index}}}")
}

/// Runs one operation. `root` is the repository root for git commands.
pub fn run(repo: &Repository, root: &Path, op: Op) -> Result<Value> {
    match op {
        Op::BranchCreate { name, at, checkout } => {
            check_branch_name(&name)?;
            let commit = resolve(repo, at.as_deref())?;
            repo.branch(&name, &commit, false).map_err(|e| match e.code() {
                git2::ErrorCode::Exists => anyhow!("A branch named '{name}' already exists"),
                _ => anyhow!(e),
            })?;
            if checkout {
                // The branch exists now; if the checkout fails (local changes in the way), it stays.
                gitcli::run(root, &["checkout", &name], None)?;
            }
            Ok(json!({ "name": name, "oid": commit.id().to_string() }))
        }
        Op::BranchRename { name, to } => {
            check_branch_name(&to)?;
            let mut branch = repo.find_branch(&name, BranchType::Local).with_context(|| format!("Branch '{name}' not found"))?;
            branch.rename(&to, false).map_err(|e| match e.code() {
                git2::ErrorCode::Exists => anyhow!("A branch named '{to}' already exists"),
                _ => anyhow!(e),
            })?;
            Ok(json!({ "name": to }))
        }
        Op::BranchDelete { name, force } => {
            let mut branch = repo.find_branch(&name, BranchType::Local).with_context(|| format!("Branch '{name}' not found"))?;
            if branch.is_head() {
                bail!("'{name}' is the current branch. Check out another branch first.");
            }
            let tip = branch.get().peel_to_commit()?.id();
            let lost = commits_only_on(repo, tip, &format!("refs/heads/{name}"), 200)?;
            if !lost.is_empty() && !force {
                return Ok(json!({ "needsForce": true, "oid": tip.to_string(), "commits": lost }));
            }
            branch.delete()?;
            Ok(json!({ "deleted": name, "oid": tip.to_string() }))
        }
        Op::BranchRestore { name, oid } => {
            check_branch_name(&name)?;
            let commit = repo.find_commit(parse_oid(&oid)?)?;
            repo.branch(&name, &commit, false)?;
            Ok(json!({ "name": name }))
        }
        Op::SetUpstream { branch, upstream } => {
            let mut b = repo.find_branch(&branch, BranchType::Local).with_context(|| format!("Branch '{branch}' not found"))?;
            b.set_upstream(upstream.as_deref())?;
            Ok(json!({ "ok": true }))
        }
        Op::Checkout { target, kind } => {
            match kind.as_str() {
                "branch" => {
                    gitcli::run(root, &["checkout", &target], None)?;
                }
                "remote" => {
                    // `origin/feature/x` becomes the local branch `feature/x` that tracks it.
                    let remotes = repo.remotes()?;
                    let local = remotes
                        .iter()
                        .flatten()
                        .flatten()
                        .filter_map(|r| target.strip_prefix(r).and_then(|rest| rest.strip_prefix('/')))
                        .min_by_key(|rest| rest.len())
                        .ok_or_else(|| anyhow!("'{target}' is not a remote branch"))?
                        .to_string();
                    if repo.find_branch(&local, BranchType::Local).is_ok() {
                        gitcli::run(root, &["checkout", &local], None)?;
                    } else {
                        gitcli::run(root, &["checkout", "-b", &local, "--track", &target], None)?;
                    }
                    return Ok(json!({ "branch": local }));
                }
                _ => {
                    let commit = resolve(repo, Some(&target))?;
                    gitcli::run(root, &["checkout", "--detach", &commit.id().to_string()], None)?;
                }
            }
            Ok(json!({ "ok": true }))
        }
        Op::TagCreate { name, at, message } => {
            if !git2::Reference::is_valid_name(&format!("refs/tags/{name}")) {
                bail!("'{name}' is not a valid tag name");
            }
            let commit = resolve(repo, at.as_deref())?;
            let made = match message.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
                Some(msg) => {
                    let sig =
                        repo.signature().context("Set user.name and user.email in your Git configuration to create an annotated tag")?;
                    repo.tag(&name, commit.as_object(), &sig, msg, false)
                }
                None => repo.tag_lightweight(&name, commit.as_object(), false),
            };
            made.map_err(|e| match e.code() {
                git2::ErrorCode::Exists => anyhow!("A tag named '{name}' already exists"),
                _ => anyhow!(e),
            })?;
            Ok(json!({ "name": name, "oid": commit.id().to_string() }))
        }
        Op::TagDelete { name } => {
            let target = repo.find_reference(&format!("refs/tags/{name}")).with_context(|| format!("Tag '{name}' not found"))?.target();
            repo.tag_delete(&name)?;
            Ok(json!({ "deleted": name, "oid": target.map(|o| o.to_string()) }))
        }
        Op::TagRestore { name, oid } => {
            repo.reference(&format!("refs/tags/{name}"), parse_oid(&oid)?, false, "restore tag")?;
            Ok(json!({ "name": name }))
        }
        Op::Merge { branch, into } => {
            let current = crate::repo::head(repo).branch;
            if let Some(into) = into.filter(|i| current.as_deref() != Some(i.as_str())) {
                gitcli::run(root, &["checkout", &into], None)?;
            }
            let before = Repository::open(root)?.head()?.peel_to_commit()?.id().to_string();
            in_progress(root, gitcli::run(root, &["merge", "--no-edit", &branch], None), &before)
        }
        Op::Rebase { branch, onto } => {
            let before = match &branch {
                Some(b) => resolve(repo, Some(b))?.id().to_string(),
                None => resolve(repo, None)?.id().to_string(),
            };
            let mut args = vec!["rebase", onto.as_str()];
            if let Some(b) = &branch {
                args.push(b);
            }
            in_progress(root, gitcli::run(root, &args, None), &before)
        }
        Op::StashSave { message, untracked, keep_index } => {
            let mut args = vec!["stash", "push"];
            if untracked {
                args.push("--include-untracked");
            }
            if keep_index {
                args.push("--keep-index");
            }
            let msg = message.unwrap_or_default();
            if !msg.trim().is_empty() {
                args.extend(["-m", msg.trim()]);
            }
            let out = gitcli::run(root, &args, None)?;
            if out.contains("No local changes to save") {
                bail!("There are no local changes to stash");
            }
            Ok(json!({ "ok": true }))
        }
        Op::StashApply { index } => {
            gitcli::run(root, &["stash", "apply", &stash_spec(index)], None)?;
            Ok(json!({ "ok": true }))
        }
        Op::StashPop { index } => {
            gitcli::run(root, &["stash", "pop", &stash_spec(index)], None)?;
            Ok(json!({ "ok": true }))
        }
        Op::StashDrop { index } => {
            let (oid, message) =
                crate::history::stashes(repo).into_iter().nth(index).ok_or_else(|| anyhow!("There is no stash {index}"))?;
            // Dropping edits the reflog of refs/stash; libgit2 needs its own mutable handle.
            let mut own = Repository::open(root)?;
            own.stash_drop(index)?;
            Ok(json!({ "oid": oid.to_string(), "message": message }))
        }
        Op::StashStore { oid, message } => {
            let oid = parse_oid(&oid)?;
            let commit = repo.find_commit(oid)?;
            if commit.parent_count() < 2 {
                bail!("{oid} is not a stash commit");
            }
            // The same as `git stash store`: the new entry becomes stash@{0}.
            repo.reference("refs/stash", oid, true, &message)?;
            Ok(json!({ "ok": true }))
        }
        Op::RemoteAdd { name, url } => {
            if url.trim().is_empty() {
                bail!("Enter the URL of the remote");
            }
            repo.remote(&name, url.trim()).map_err(|e| match e.code() {
                git2::ErrorCode::Exists => anyhow!("A remote named '{name}' already exists"),
                git2::ErrorCode::InvalidSpec => anyhow!("'{name}' is not a valid remote name"),
                _ => anyhow!(e),
            })?;
            Ok(json!({ "ok": true }))
        }
        Op::RemoteRemove { name } => {
            repo.remote_delete(&name).with_context(|| format!("Cannot remove the remote '{name}'"))?;
            Ok(json!({ "ok": true }))
        }
        Op::RemoteRename { name, to } => {
            let problems = repo.remote_rename(&name, &to).with_context(|| format!("Cannot rename the remote '{name}'"))?;
            let skipped: Vec<String> = problems.iter().flatten().flatten().map(str::to_string).collect();
            Ok(json!({ "ok": true, "refspecsNotRenamed": skipped }))
        }
        Op::RemoteSetUrl { name, url } => {
            repo.find_remote(&name).with_context(|| format!("Remote '{name}' not found"))?;
            repo.remote_set_url(&name, url.trim())?;
            Ok(json!({ "ok": true }))
        }
    }
}

/// Turns the result of a command that can stop for conflicts into the answer for the UI.
/// A stop with an operation in progress is not an error: the user resolves the conflicts
/// and continues. `before` is the commit to return to when the operation is undone.
pub fn in_progress(root: &Path, result: Result<String, gitcli::GitError>, before: &str) -> Result<Value> {
    match result {
        Ok(out) => Ok(json!({ "before": before, "output": out.trim() })),
        Err(e) => match crate::repo::state_name(Repository::open(root)?.state()) {
            Some(state) => Ok(json!({ "before": before, "stopped": state, "output": e.output, "command": e.command })),
            None => Err(e.into()),
        },
    }
}

// ---------------------------------------------------------------- network jobs

/// A network operation, as the UI sends it.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "job", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum NetJob {
    Fetch {
        /// One remote; all remotes when absent.
        remote: Option<String>,
        #[serde(default)]
        prune: bool,
    },
    Pull {
        /// `ff-only`, `merge` or `rebase`; the user's git configuration decides when absent.
        mode: Option<String>,
    },
    Push {
        remote: String,
        /// Local branch or tag to push.
        refname: String,
        /// Name on the remote when it differs from `refname`.
        remote_name: Option<String>,
        #[serde(default)]
        set_upstream: bool,
        /// Sent as `--force-with-lease`, never as a plain force.
        #[serde(default)]
        force: bool,
        #[serde(default)]
        tags: bool,
    },
    /// Deletes a branch or tag on the remote.
    PushDelete { remote: String, refname: String },
}

impl NetJob {
    /// The title for the UI and the git arguments.
    pub fn command(&self) -> (String, Vec<String>) {
        let s = |v: &str| v.to_string();
        match self {
            NetJob::Fetch { remote, prune } => {
                let mut args = vec![s("fetch"), s("--progress")];
                if *prune {
                    args.push(s("--prune"));
                }
                match remote {
                    Some(r) => args.push(r.clone()),
                    None => args.push(s("--all")),
                }
                (remote.as_ref().map_or(s("Fetch"), |r| format!("Fetch {r}")), args)
            }
            NetJob::Pull { mode } => {
                let mut args = vec![s("pull"), s("--progress")];
                match mode.as_deref() {
                    Some("ff-only") => args.push(s("--ff-only")),
                    Some("merge") => args.push(s("--no-rebase")),
                    Some("rebase") => args.push(s("--rebase")),
                    _ => {}
                }
                (s("Pull"), args)
            }
            NetJob::Push { remote, refname, remote_name, set_upstream, force, tags } => {
                let mut args = vec![s("push"), s("--progress")];
                if *set_upstream {
                    args.push(s("--set-upstream"));
                }
                if *force {
                    // Refuses to overwrite commits that the local repository has not seen.
                    args.push(s("--force-with-lease"));
                }
                if *tags {
                    args.push(s("--tags"));
                }
                args.push(remote.clone());
                args.push(match remote_name {
                    Some(n) if n != refname => format!("{refname}:{n}"),
                    _ => refname.clone(),
                });
                (format!("Push {refname}"), args)
            }
            NetJob::PushDelete { remote, refname } => {
                (format!("Delete {remote}/{refname}"), vec![s("push"), s("--progress"), remote.clone(), s("--delete"), refname.clone()])
            }
        }
    }
}

/// The folder name git would choose for a clone of `url`.
pub fn clone_dir_name(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches(['/', '\\']);
    let last = trimmed.rsplit(['/', '\\', ':']).next().unwrap_or("");
    let name = last.strip_suffix(".git").unwrap_or(last);
    if name.is_empty() {
        "repository".to_string()
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_push_always_uses_the_lease() {
        let job = NetJob::Push {
            remote: "origin".into(),
            refname: "main".into(),
            remote_name: None,
            set_upstream: false,
            force: true,
            tags: false,
        };
        let (_, args) = job.command();
        assert_eq!(args, ["push", "--progress", "--force-with-lease", "origin", "main"]);
        assert!(!args.iter().any(|a| a == "--force" || a == "-f"));
    }

    #[test]
    fn push_arguments() {
        let job = NetJob::Push {
            remote: "origin".into(),
            refname: "topic".into(),
            remote_name: Some("review/topic".into()),
            set_upstream: true,
            force: false,
            tags: true,
        };
        assert_eq!(job.command().1, ["push", "--progress", "--set-upstream", "--tags", "origin", "topic:review/topic"]);
        let del = NetJob::PushDelete { remote: "origin".into(), refname: "old".into() };
        assert_eq!(del.command().1, ["push", "--progress", "origin", "--delete", "old"]);
    }

    #[test]
    fn fetch_and_pull_arguments() {
        assert_eq!(NetJob::Fetch { remote: None, prune: true }.command().1, ["fetch", "--progress", "--prune", "--all"]);
        assert_eq!(NetJob::Fetch { remote: Some("upstream".into()), prune: false }.command().1, ["fetch", "--progress", "upstream"]);
        assert_eq!(NetJob::Pull { mode: None }.command().1, ["pull", "--progress"]);
        assert_eq!(NetJob::Pull { mode: Some("rebase".into()) }.command().1, ["pull", "--progress", "--rebase"]);
        assert_eq!(NetJob::Pull { mode: Some("ff-only".into()) }.command().1, ["pull", "--progress", "--ff-only"]);
    }

    #[test]
    fn clone_folder_name_follows_git() {
        assert_eq!(clone_dir_name("https://github.com/git/git.git"), "git");
        assert_eq!(clone_dir_name("git@example.com:team/project.git"), "project");
        assert_eq!(clone_dir_name("https://example.com/a/b/"), "b");
        assert_eq!(clone_dir_name("C:\\repos\\thing.git"), "thing");
        assert_eq!(clone_dir_name(""), "repository");
    }
}
