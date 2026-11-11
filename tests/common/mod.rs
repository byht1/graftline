//! Test repositories. Every test builds its own repository in a temporary directory with
//! libgit2 and explicit signatures, so no test reads or changes a real repository or the
//! user's git configuration.

#![allow(dead_code)]

use git2::{Oid, Repository, Signature, Time};
use std::path::{Path, PathBuf};
use std::sync::Once;

static ISOLATE: Once = Once::new();

/// Points libgit2's global, XDG and system configuration at an empty directory.
pub fn isolate_git_config() {
    ISOLATE.call_once(|| {
        let empty = std::env::temp_dir().join(format!("graftline-test-config-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        for level in [git2::ConfigLevel::Global, git2::ConfigLevel::XDG, git2::ConfigLevel::System, git2::ConfigLevel::ProgramData] {
            // Safety: called once, before any repository is opened by this test binary.
            unsafe { git2::opts::set_search_path(level, &empty).unwrap() };
        }
        // The same for the `git` executable that some operations run.
        std::env::set_var("GIT_CONFIG_GLOBAL", empty.join("gitconfig"));
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    });
}

pub struct TestRepo {
    pub dir: tempfile::TempDir,
    pub repo: Repository,
    clock: std::cell::Cell<i64>,
}

impl TestRepo {
    pub fn new() -> TestRepo {
        isolate_git_config();
        let dir = tempfile::tempdir().unwrap();
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = Repository::init_opts(dir.path(), &opts).unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", "Test User").unwrap();
            cfg.set_str("user.email", "test@example.com").unwrap();
            cfg.set_bool("commit.gpgsign", false).unwrap();
            cfg.set_bool("core.autocrlf", false).unwrap();
        }
        TestRepo { dir, repo, clock: std::cell::Cell::new(1_700_000_000) }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The root the app uses for this repository.
    pub fn root(&self) -> PathBuf {
        graftline::repo::discover(self.path()).unwrap()
    }

    fn tick(&self) -> i64 {
        let t = self.clock.get() + 60;
        self.clock.set(t);
        t
    }

    pub fn sig(&self, name: &str) -> Signature<'static> {
        Signature::new(name, &format!("{}@example.com", name.to_lowercase().replace(' ', ".")), &Time::new(self.tick(), 0)).unwrap()
    }

    pub fn write(&self, rel: &str, content: &str) {
        let p = self.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    pub fn write_bytes(&self, rel: &str, content: &[u8]) {
        let p = self.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path().join(rel)).unwrap()
    }

    pub fn remove(&self, rel: &str) {
        std::fs::remove_file(self.path().join(rel)).unwrap();
    }

    /// Stages everything in the working tree (additions, changes, deletions).
    pub fn stage_all(&self) {
        let mut index = self.repo.index().unwrap();
        index.add_all(["*"], git2::IndexAddOption::DEFAULT, None).unwrap();
        index.update_all(["*"], None).unwrap();
        index.write().unwrap();
    }

    /// Commits the index on HEAD. Returns the new commit.
    pub fn commit(&self, message: &str) -> Oid {
        self.commit_as("Test User", message)
    }

    pub fn commit_as(&self, author: &str, message: &str) -> Oid {
        let mut index = self.repo.index().unwrap();
        let tree = self.repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = self.sig(author);
        let parents: Vec<git2::Commit> = self.repo.head().ok().and_then(|h| h.peel_to_commit().ok()).into_iter().collect();
        let refs: Vec<&git2::Commit> = parents.iter().collect();
        self.repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &refs).unwrap()
    }

    /// Writes a file, stages everything and commits.
    pub fn commit_file(&self, rel: &str, content: &str, message: &str) -> Oid {
        self.write(rel, content);
        self.stage_all();
        self.commit(message)
    }

    /// Creates a commit object with the given parents and the tree of the first parent plus
    /// one file named after the message. Does not move any ref.
    pub fn raw_commit(&self, message: &str, parents: &[Oid]) -> Oid {
        let sig = self.sig("Test User");
        let parent_commits: Vec<git2::Commit> = parents.iter().map(|p| self.repo.find_commit(*p).unwrap()).collect();
        let base = parent_commits.first().map(|c| c.tree().unwrap());
        let blob = self.repo.blob(message.as_bytes()).unwrap();
        let mut builder = self.repo.treebuilder(base.as_ref()).unwrap();
        let name: String = message.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        builder.insert(format!("{name}.txt"), blob, 0o100644).unwrap();
        let tree = self.repo.find_tree(builder.write().unwrap()).unwrap();
        let refs: Vec<&git2::Commit> = parent_commits.iter().collect();
        self.repo.commit(None, &sig, &sig, message, &tree, &refs).unwrap()
    }

    pub fn branch(&self, name: &str, target: Oid) {
        self.repo.branch(name, &self.repo.find_commit(target).unwrap(), true).unwrap();
    }

    pub fn set_ref(&self, name: &str, target: Oid) {
        self.repo.reference(name, target, true, "test").unwrap();
    }

    /// Makes `branch` the current branch and updates index and working tree to it.
    pub fn checkout(&self, branch: &str) {
        self.repo.set_head(&format!("refs/heads/{branch}")).unwrap();
        self.repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force())).unwrap();
    }

    pub fn head(&self) -> Oid {
        self.repo.head().unwrap().peel_to_commit().unwrap().id()
    }

    pub fn tag(&self, name: &str, target: Oid, annotated: bool) {
        let obj = self.repo.find_object(target, None).unwrap();
        if annotated {
            self.repo.tag(name, &obj, &self.sig("Test User"), &format!("Release {name}"), false).unwrap();
        } else {
            self.repo.tag_lightweight(name, &obj, false).unwrap();
        }
    }
}
