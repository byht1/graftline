//! Safety snapshots: before the app discards or overwrites files in the working tree, it
//! saves their current content as a commit under `refs/graftline/snapshots/`. The user can
//! restore a snapshot, so a wrong click does not cost the work.
//!
//! A snapshot holds only the files that were about to change, not the whole tree. The refs
//! keep the objects alive; they are not branches or tags and do not appear in the graph.

use anyhow::{anyhow, Context, Result};
use git2::{Oid, Repository};
use serde::Serialize;
use std::path::Path;

pub const REF_PREFIX: &str = "refs/graftline/snapshots/";
/// Older snapshots are removed when there are more than this.
const KEEP: usize = 50;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// The ref name without the prefix.
    pub name: String,
    pub oid: String,
    /// Why the snapshot was taken, for example "Before discard".
    pub reason: String,
    /// Seconds since the Unix epoch.
    pub time: i64,
    pub files: Vec<String>,
}

fn signature(repo: &Repository) -> git2::Signature<'static> {
    repo.signature().or_else(|_| git2::Signature::now("Graftline", "graftline@localhost")).expect("a fixed signature is valid")
}

fn file_mode(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 != 0 {
            return 0o100755;
        }
    }
    let _ = meta;
    0o100644
}

/// Saves the current content of `paths` (relative to the working directory). Paths that do
/// not exist as files are skipped. Returns `None` when there was nothing to save.
pub fn snapshot(repo: &Repository, paths: &[String], reason: &str) -> Result<Option<Snapshot>> {
    let workdir = repo.workdir().ok_or_else(|| anyhow!("A bare repository has no working tree"))?;
    let mut index = git2::Index::new()?;
    let mut files = Vec::new();
    for rel in paths {
        let abs = workdir.join(rel);
        let Ok(meta) = std::fs::symlink_metadata(&abs) else { continue };
        let (id, mode) = if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&abs)?;
            (repo.blob(target.to_string_lossy().as_bytes())?, 0o120000)
        } else if meta.is_file() {
            (repo.blob_path(&abs).with_context(|| format!("Cannot read {rel}"))?, file_mode(&meta))
        } else {
            continue;
        };
        index.add(&git2::IndexEntry {
            ctime: git2::IndexTime::new(0, 0),
            mtime: git2::IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode,
            uid: 0,
            gid: 0,
            file_size: 0,
            id,
            flags: 0,
            flags_extended: 0,
            path: rel.replace('\\', "/").into_bytes(),
        })?;
        files.push(rel.replace('\\', "/"));
    }
    if files.is_empty() {
        return Ok(None);
    }
    let tree = repo.find_tree(index.write_tree_to(repo)?)?;
    let sig = signature(repo);
    let parents: Vec<git2::Commit> = repo.head().ok().and_then(|h| h.peel_to_commit().ok()).into_iter().collect();
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
    let message = format!("{reason}\n\n{}", files.join("\n"));
    let oid = repo.commit(None, &sig, &sig, &message, &tree, &parent_refs)?;
    // The name sorts by time; the short id keeps two snapshots of one second apart.
    let name = format!("{:012}-{}", sig.when().seconds(), &oid.to_string()[..8]);
    repo.reference(&format!("{REF_PREFIX}{name}"), oid, true, reason)?;
    prune(repo)?;
    Ok(Some(Snapshot { name, oid: oid.to_string(), reason: reason.to_string(), time: sig.when().seconds(), files }))
}

fn tree_files(repo: &Repository, oid: Oid) -> Result<Vec<String>> {
    let tree = repo.find_commit(oid)?.tree()?;
    let mut files = Vec::new();
    tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
        if entry.kind() == Some(git2::ObjectType::Blob) {
            files.push(format!("{dir}{}", String::from_utf8_lossy(entry.name_bytes())));
        }
        git2::TreeWalkResult::Ok
    })?;
    Ok(files)
}

/// All snapshots, newest first.
pub fn list(repo: &Repository) -> Result<Vec<Snapshot>> {
    let mut out = Vec::new();
    for r in repo.references_glob(&format!("{REF_PREFIX}*"))? {
        let Ok(r) = r else { continue };
        let (Ok(full), Some(oid)) = (r.name(), r.target()) else { continue };
        let Ok(commit) = repo.find_commit(oid) else { continue };
        let reason = commit.summary().ok().flatten().unwrap_or("").to_string();
        out.push(Snapshot {
            name: full[REF_PREFIX.len()..].to_string(),
            oid: oid.to_string(),
            reason,
            time: commit.time().seconds(),
            files: tree_files(repo, oid).unwrap_or_default(),
        });
    }
    out.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(out)
}

fn prune(repo: &Repository) -> Result<()> {
    for old in list(repo)?.into_iter().skip(KEEP) {
        if let Ok(mut r) = repo.find_reference(&format!("{REF_PREFIX}{}", old.name)) {
            let _ = r.delete();
        }
    }
    Ok(())
}

fn find(repo: &Repository, name: &str) -> Result<Oid> {
    if name.contains("..") || name.contains('/') || name.is_empty() {
        return Err(anyhow!("'{name}' is not a snapshot name"));
    }
    repo.find_reference(&format!("{REF_PREFIX}{name}")).ok().and_then(|r| r.target()).ok_or_else(|| anyhow!("Snapshot {name} not found"))
}

/// Writes the files of a snapshot back into the working tree, replacing what is there.
/// The files that are replaced are saved in a new snapshot first. Returns the restored paths.
pub fn restore(repo: &Repository, name: &str) -> Result<Vec<String>> {
    let oid = find(repo, name)?;
    let workdir = repo.workdir().ok_or_else(|| anyhow!("A bare repository has no working tree"))?.to_path_buf();
    let files = tree_files(repo, oid)?;
    snapshot(repo, &files, "Before restoring a snapshot")?;
    let tree = repo.find_commit(oid)?.tree()?;
    for rel in &files {
        let entry = tree.get_path(Path::new(rel))?;
        let blob = repo.find_blob(entry.id())?;
        let abs = workdir.join(rel);
        if let Some(dir) = abs.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("Cannot create {}", dir.display()))?;
        }
        if entry.filemode() == 0o120000 {
            let _ = std::fs::remove_file(&abs);
            #[cfg(unix)]
            std::os::unix::fs::symlink(String::from_utf8_lossy(blob.content()).as_ref(), &abs)?;
            #[cfg(not(unix))]
            std::fs::write(&abs, blob.content())?;
        } else {
            std::fs::write(&abs, blob.content()).with_context(|| format!("Cannot write {rel}"))?;
            #[cfg(unix)]
            if entry.filemode() == 0o100755 {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&abs, std::fs::Permissions::from_mode(0o755))?;
            }
        }
    }
    Ok(files)
}

pub fn drop(repo: &Repository, name: &str) -> Result<()> {
    find(repo, name)?;
    repo.find_reference(&format!("{REF_PREFIX}{name}"))?.delete()?;
    Ok(())
}
