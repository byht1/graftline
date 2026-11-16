//! The working tree and the index: status, diffs, staging by file, hunk and line, discard,
//! commit.
//!
//! Partial staging builds a patch from the selected lines of the current diff and applies it
//! with libgit2. Everything that removes work from the working tree saves a snapshot first
//! (see `safety.rs`).

use crate::repo::{self, FileChange, FileDiff};
use crate::{gitcli, safety};
use anyhow::{anyhow, bail, Context, Result};
use git2::{ApplyLocation, Delta, Diff, DiffOptions, Patch, Repository, Status, StatusOptions};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// HEAD compared with the index.
    Staged,
    /// The index compared with the working tree.
    Unstaged,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkStatus {
    pub staged: Vec<FileChange>,
    pub unstaged: Vec<FileChange>,
    /// Files with merge conflicts. They also appear in `unstaged`.
    pub conflicted: Vec<String>,
}

impl WorkStatus {
    pub fn is_clean(&self) -> bool {
        self.staged.is_empty() && self.unstaged.is_empty()
    }
}

fn status_options() -> StatusOptions {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true).include_ignored(false).renames_head_to_index(true).exclude_submodules(false);
    opts
}

fn path_of(p: Option<&Path>) -> String {
    p.map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default()
}

fn change(delta: &git2::DiffDelta, status: char) -> FileChange {
    let new_path = path_of(delta.new_file().path());
    let old_path = path_of(delta.old_file().path());
    let path = if status == 'D' { old_path.clone() } else { new_path };
    FileChange {
        old_path: (status == 'R' && old_path != path).then_some(old_path),
        path,
        status,
        additions: None,
        deletions: None,
        binary: false,
    }
}

/// True when the working tree or the index differs from HEAD (untracked files count).
pub fn is_dirty(repo: &Repository) -> bool {
    if repo.is_bare() {
        return false;
    }
    repo.statuses(Some(&mut status_options())).map(|s| !s.is_empty()).unwrap_or(false)
}

pub fn status(repo: &Repository) -> Result<WorkStatus> {
    let mut out = WorkStatus::default();
    if repo.is_bare() {
        return Ok(out);
    }
    let statuses = repo.statuses(Some(&mut status_options()))?;
    for entry in statuses.iter() {
        let s = entry.status();
        if s.contains(Status::CONFLICTED) {
            let path = String::from_utf8_lossy(entry.path_bytes()).replace('\\', "/");
            out.unstaged.push(FileChange {
                path: path.clone(),
                old_path: None,
                status: 'U',
                additions: None,
                deletions: None,
                binary: false,
            });
            out.conflicted.push(path);
            continue;
        }
        let staged = if s.contains(Status::INDEX_NEW) {
            Some('A')
        } else if s.contains(Status::INDEX_DELETED) {
            Some('D')
        } else if s.contains(Status::INDEX_RENAMED) {
            Some('R')
        } else if s.contains(Status::INDEX_TYPECHANGE) {
            Some('T')
        } else if s.contains(Status::INDEX_MODIFIED) {
            Some('M')
        } else {
            None
        };
        if let (Some(c), Some(delta)) = (staged, entry.head_to_index()) {
            out.staged.push(change(&delta, c));
        }
        let unstaged = if s.contains(Status::WT_NEW) {
            Some('?')
        } else if s.contains(Status::WT_DELETED) {
            Some('D')
        } else if s.contains(Status::WT_RENAMED) {
            Some('R')
        } else if s.contains(Status::WT_TYPECHANGE) {
            Some('T')
        } else if s.contains(Status::WT_MODIFIED) {
            Some('M')
        } else {
            None
        };
        if let (Some(c), Some(delta)) = (unstaged, entry.index_to_workdir()) {
            out.unstaged.push(change(&delta, c));
        }
    }
    out.staged.sort_by(|a, b| a.path.cmp(&b.path));
    out.unstaged.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn head_tree(repo: &Repository) -> Option<git2::Tree<'_>> {
    repo.head().ok()?.peel_to_tree().ok()
}

/// The diff of one side, limited to `paths` when given.
pub fn side_diff<'r>(repo: &'r Repository, side: Side, paths: &[&str]) -> Result<Diff<'r>> {
    let mut opts = DiffOptions::new();
    opts.context_lines(3).include_untracked(true).recurse_untracked_dirs(true).show_untracked_content(true).include_typechange(true);
    if !paths.is_empty() {
        opts.disable_pathspec_match(true);
        for p in paths {
            opts.pathspec(p);
        }
    }
    let mut diff = match side {
        Side::Staged => repo.diff_tree_to_index(head_tree(repo).as_ref(), None, Some(&mut opts))?,
        Side::Unstaged => repo.diff_index_to_workdir(None, Some(&mut opts))?,
    };
    if side == Side::Staged {
        diff.find_similar(Some(git2::DiffFindOptions::new().renames(true)))?;
    }
    Ok(diff)
}

pub fn file_diff(repo: &Repository, side: Side, path: &str, old_path: Option<&str>) -> Result<FileDiff> {
    let mut paths = vec![path];
    if let Some(old) = old_path {
        paths.push(old);
    }
    let diff = side_diff(repo, side, &paths)?;
    repo::file_diff(&diff, path)
}

// ---------------------------------------------------------------- whole files

/// Stages the working-tree state of each path: an added or changed file, or a deletion.
pub fn stage(repo: &Repository, paths: &[String]) -> Result<()> {
    let workdir = repo.workdir().ok_or_else(|| anyhow!("A bare repository has no working tree"))?;
    let mut index = repo.index()?;
    for p in paths {
        let rel = Path::new(p);
        if std::fs::symlink_metadata(workdir.join(rel)).is_ok() {
            index.add_path(rel).with_context(|| format!("Cannot stage {p}"))?;
        } else {
            index.remove_path(rel).with_context(|| format!("Cannot stage the deletion of {p}"))?;
        }
    }
    index.write()?;
    Ok(())
}

/// Makes the index entry of each path equal to HEAD again. The working tree is not touched.
pub fn unstage(repo: &Repository, paths: &[String]) -> Result<()> {
    match repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
        Some(head) => repo.reset_default(Some(head.as_object()), paths.iter().map(String::as_str))?,
        None => {
            // No commit yet: unstaging means removing the entry from the index.
            let mut index = repo.index()?;
            for p in paths {
                index.remove_path(Path::new(p))?;
            }
            index.write()?;
        }
    }
    Ok(())
}

/// Throws away the unstaged changes of `paths`: tracked files return to their index version,
/// untracked files are deleted. The current content is saved in a snapshot first.
pub fn discard(repo: &Repository, paths: &[String]) -> Result<Option<safety::Snapshot>> {
    let workdir = repo.workdir().ok_or_else(|| anyhow!("A bare repository has no working tree"))?.to_path_buf();
    let snapshot = safety::snapshot(repo, paths, "Before discard")?;
    let index = repo.index()?;
    let mut tracked = Vec::new();
    for p in paths {
        if index.get_path(Path::new(p), 0).is_some() {
            tracked.push(p.as_str());
        } else {
            let abs = workdir.join(p);
            if std::fs::symlink_metadata(&abs).is_ok() {
                std::fs::remove_file(&abs).with_context(|| format!("Cannot delete {p}"))?;
                // Remove the folders that became empty, up to the repository root.
                let mut dir = abs.parent();
                while let Some(d) = dir {
                    if d == workdir || std::fs::remove_dir(d).is_err() {
                        break;
                    }
                    dir = d.parent();
                }
            }
        }
    }
    if !tracked.is_empty() {
        let mut co = git2::build::CheckoutBuilder::new();
        co.force().update_index(false);
        for p in &tracked {
            co.path(*p);
        }
        repo.checkout_index(None, Some(&mut co))?;
    }
    Ok(snapshot)
}

// ---------------------------------------------------------------- hunks and lines

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PatchOp {
    /// Apply selected unstaged changes to the index.
    Stage,
    /// Remove selected staged changes from the index.
    Unstage,
    /// Remove selected unstaged changes from the working tree.
    Discard,
}

/// A hunk of the current diff, with the lines to act on.
#[derive(Debug, Clone, Deserialize)]
pub struct HunkSelection {
    /// Position of the hunk in the file diff.
    pub index: usize,
    /// The hunk header the UI saw. A different header means the file changed since.
    pub header: String,
    /// Line positions inside the hunk; `None` selects the whole hunk.
    pub lines: Option<Vec<usize>>,
}

struct Emitted<'a> {
    sign: u8,
    content: &'a [u8],
    /// "\ No newline at end of file" follows this line.
    no_newline: bool,
}

/// Builds the lines of one partial hunk.
///
/// Forward (stage): the patch is applied to the old side of the diff. A selected line keeps
/// its sign; an unselected removal stays as context; an unselected addition is left out.
///
/// Reverse (unstage, discard): the patch is applied to the new side and must undo the
/// selected lines. A selected addition becomes a removal and the other way round; an
/// unselected addition is context there; an unselected removal is left out.
fn partial_hunk<'a>(lines: &[(char, &'a [u8])], selected: &dyn Fn(usize) -> bool, reverse: bool) -> Vec<Emitted<'a>> {
    let mut out: Vec<Emitted> = Vec::with_capacity(lines.len());
    let mut last_kept = false;
    for (i, &(origin, content)) in lines.iter().enumerate() {
        let sign = match origin {
            ' ' => Some(b' '),
            '-' if selected(i) => Some(if reverse { b'+' } else { b'-' }),
            '-' => (!reverse).then_some(b' '),
            '+' if selected(i) => Some(if reverse { b'-' } else { b'+' }),
            '+' => reverse.then_some(b' '),
            _ => {
                // An end-of-file marker belongs to the line before it.
                if last_kept {
                    if let Some(prev) = out.last_mut() {
                        prev.no_newline = true;
                    }
                }
                continue;
            }
        };
        last_kept = sign.is_some();
        if let Some(sign) = sign {
            out.push(Emitted { sign, content, no_newline: false });
        }
    }
    // Inside a run of changed lines, removals come before additions.
    let mut i = 0;
    while i < out.len() {
        if out[i].sign == b' ' {
            i += 1;
            continue;
        }
        let end = out[i..].iter().position(|e| e.sign == b' ').map_or(out.len(), |n| i + n);
        out[i..end].sort_by_key(|e| e.sign != b'-');
        i = end;
    }
    out
}

/// Quotes a path for a patch header when it has characters that need it.
fn patch_path(prefix: &str, path: &str) -> String {
    let plain = path.bytes().all(|b| (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\');
    if plain {
        return format!("{prefix}{path}");
    }
    let mut s = format!("\"{prefix}");
    for b in path.bytes() {
        match b {
            b'"' => s.push_str("\\\""),
            b'\\' => s.push_str("\\\\"),
            b'\t' => s.push_str("\\t"),
            b'\n' => s.push_str("\\n"),
            0x20..=0x7e => s.push(b as char),
            _ => s.push_str(&format!("\\{b:03o}")),
        }
    }
    s.push('"');
    s
}

/// Builds a patch with the selected lines of `patch`. Returns `None` when the selection
/// contains no changed line.
fn build_patch(patch: &Patch, path: &str, hunks: &[HunkSelection], reverse: bool, new_file: bool) -> Result<Option<Vec<u8>>> {
    let mut body: Vec<u8> = Vec::new();
    let mut delta: i64 = 0;
    let mut sorted: Vec<&HunkSelection> = hunks.iter().collect();
    sorted.sort_by_key(|h| h.index);
    for sel in sorted {
        if sel.index >= patch.num_hunks() {
            bail!("The file changed since the diff was shown. Reload and try again.");
        }
        let (hunk, count) = patch.hunk(sel.index)?;
        if String::from_utf8_lossy(hunk.header()).trim_end() != sel.header.trim_end() {
            bail!("The file changed since the diff was shown. Reload and try again.");
        }
        let mut lines: Vec<(char, &[u8])> = Vec::with_capacity(count);
        for l in 0..count {
            let line = patch.line_in_hunk(sel.index, l)?;
            lines.push((line.origin(), line.content()));
        }
        let selected = |i: usize| sel.lines.as_ref().is_none_or(|ls| ls.contains(&i));
        let emitted = partial_hunk(&lines, &selected, reverse);
        if emitted.iter().all(|e| e.sign == b' ') {
            continue;
        }
        let old_count = emitted.iter().filter(|e| e.sign != b'+').count() as i64;
        let new_count = emitted.iter().filter(|e| e.sign != b'-').count() as i64;
        // The patch is applied to the old side when forward, to the new side when reverse.
        let old_start = if reverse { hunk.new_start() } else { hunk.old_start() } as i64;
        // libgit2 looks for the hunk at exactly `new_start - 1` in the file as changed by the
        // hunks before it. An insertion without context goes after line `old_start`.
        let new_start = old_start + delta + i64::from(old_count == 0);
        body.extend_from_slice(format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@\n").as_bytes());
        for e in &emitted {
            body.push(e.sign);
            body.extend_from_slice(e.content);
            if !e.content.ends_with(b"\n") {
                body.push(b'\n');
            }
            if e.no_newline {
                body.extend_from_slice(b"\\ No newline at end of file\n");
            }
        }
        delta += new_count - old_count;
    }
    if body.is_empty() {
        return Ok(None);
    }
    let (a, b) = (patch_path("a/", path), patch_path("b/", path));
    let mut out = format!("diff --git {a} {b}\n").into_bytes();
    if new_file {
        out.extend_from_slice(format!("new file mode 100644\n--- /dev/null\n+++ {b}\n").as_bytes());
    } else {
        out.extend_from_slice(format!("--- {a}\n+++ {b}\n").as_bytes());
    }
    out.extend_from_slice(&body);
    Ok(Some(out))
}

/// Stages, unstages or discards the selected hunks or lines of one file.
/// A discard returns the snapshot that holds the file as it was.
pub fn apply_selection(repo: &Repository, op: PatchOp, path: &str, hunks: &[HunkSelection]) -> Result<Option<safety::Snapshot>> {
    let side = if op == PatchOp::Unstage { Side::Staged } else { Side::Unstaged };
    let diff = side_diff(repo, side, &[path])?;
    let index = diff
        .deltas()
        .position(|d| path_of(d.new_file().path()) == path || path_of(d.old_file().path()) == path)
        .ok_or_else(|| anyhow!("{path} has no {} changes", if side == Side::Staged { "staged" } else { "unstaged" }))?;
    let delta = diff.get_delta(index).expect("the delta was just found");
    let status = delta.status();
    let patch = Patch::from_diff(&diff, index)?.ok_or_else(|| anyhow!("{path} is a binary file: stage or discard the whole file"))?;
    if patch.delta().flags().is_binary() {
        bail!("{path} is a binary file: stage or discard the whole file");
    }
    let reverse = op != PatchOp::Stage;
    // A forward patch for a file the index does not have yet creates the file.
    let new_file = !reverse && matches!(status, Delta::Untracked | Delta::Added);
    let Some(text) = build_patch(&patch, path, hunks, reverse, new_file)? else {
        bail!("Select at least one changed line");
    };
    let snapshot = if op == PatchOp::Discard { safety::snapshot(repo, &[path.to_string()], "Before discard")? } else { None };
    let partial = Diff::from_buffer(&text).context("Cannot build the patch for the selected lines")?;
    let location = if op == PatchOp::Discard { ApplyLocation::WorkDir } else { ApplyLocation::Index };
    repo.apply(&partial, location, None)
        .with_context(|| "The selected lines do not apply. The file changed since the diff was shown: reload and try again.".to_string())?;
    Ok(snapshot)
}

// ---------------------------------------------------------------- commit

#[derive(Debug, Clone, Deserialize)]
pub struct CommitRequest {
    pub message: String,
    /// Replace the last commit instead of adding one.
    #[serde(default)]
    pub amend: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitResult {
    pub oid: String,
    /// HEAD before the commit; `None` for the first commit of a repository.
    pub previous: Option<String>,
}

/// Commits the index. With git on PATH the commit goes through `git commit`, so hooks and
/// commit signing work as the user set them up. Without git, libgit2 creates the commit
/// (no hooks, no signing).
pub fn commit(repo: &Repository, root: &Path, has_git: bool, req: &CommitRequest) -> Result<CommitResult> {
    let message = req.message.trim();
    if message.is_empty() {
        bail!("Enter a commit message");
    }
    let previous = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    if req.amend && previous.is_none() {
        bail!("There is no commit to amend yet");
    }
    if repo.index()?.has_conflicts() {
        bail!("Resolve the conflicts before you commit");
    }
    if has_git {
        let mut args = vec!["commit", "--cleanup=strip", "-F", "-"];
        if req.amend {
            args.push("--amend");
        }
        gitcli::run(root, &args, Some(message.as_bytes()))?;
    } else {
        let sig = repo.signature().context("Set user.name and user.email in your Git configuration to commit")?;
        let tree = repo.find_tree(repo.index()?.write_tree()?)?;
        match (&previous, req.amend) {
            (Some(prev), true) => {
                prev.amend(Some("HEAD"), None, Some(&sig), None, Some(message), Some(&tree))?;
            }
            (Some(prev), false) => {
                if prev.tree_id() == tree.id() && repo.state() == git2::RepositoryState::Clean {
                    bail!("Nothing is staged");
                }
                repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[prev])?;
            }
            (None, _) => {
                repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[])?;
            }
        }
    }
    // Another handle may have a stale view of HEAD; read it again.
    let head = Repository::open(root)?.head()?.peel_to_commit()?.id();
    Ok(CommitResult { oid: head.to_string(), previous: previous.map(|c| c.id().to_string()) })
}

/// Moves the current branch to `to` and keeps the index and working tree (`reset --soft`).
/// Used to undo a commit made in the app; `expect_head` guards against undoing something else.
pub fn reset_soft(repo: &Repository, to: &str, expect_head: &str) -> Result<()> {
    let head = repo.head()?.peel_to_commit()?.id();
    if head != repo::parse_oid(expect_head)? {
        bail!("HEAD moved since that commit. Nothing was undone.");
    }
    let target = repo.find_object(repo::parse_oid(to)?, Some(git2::ObjectType::Commit))?;
    repo.reset(&target, git2::ResetType::Soft, None)?;
    Ok(())
}
