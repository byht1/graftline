//! Branch, tag, stash and remote operations, and network jobs against a local remote.

mod common;

use common::TestRepo;
use graftline::gitcli::{self, GitError};
use graftline::jobs::{JobInfo, Jobs};
use graftline::ops::{self, NetJob, Op};
use graftline::{repo, worktree};
use serde_json::{json, Value};
use std::path::Path;

fn op(t: &TestRepo, value: Value) -> anyhow::Result<Value> {
    let op: Op = serde_json::from_value(value).expect("a valid operation");
    // A fresh handle, as the core opens one per call.
    ops::run(&repo::open(&t.root())?, &t.root(), op)
}

fn has_git() -> bool {
    let found = gitcli::detect().is_some();
    if !found {
        eprintln!("skipped: git is not on PATH");
    }
    found
}

fn branches(t: &TestRepo) -> Vec<String> {
    repo::refs(&repo::open(&t.root()).unwrap()).unwrap().branches.into_iter().map(|b| b.name).collect()
}

fn head_branch(t: &TestRepo) -> Option<String> {
    repo::head(&repo::open(&t.root()).unwrap()).branch
}

#[test]
fn create_rename_and_delete_a_branch() {
    let t = TestRepo::new();
    let a = t.commit_file("a.txt", "1", "first");
    let b = t.commit_file("a.txt", "2", "second");

    let made = op(&t, json!({ "op": "branch-create", "name": "topic", "at": a.to_string() })).unwrap();
    assert_eq!(made["oid"], a.to_string());
    assert_eq!(branches(&t), vec!["main", "topic"]);
    assert_eq!(head_branch(&t).as_deref(), Some("main"), "creating a branch does not switch to it");

    let err = op(&t, json!({ "op": "branch-create", "name": "topic" })).unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");
    let err = op(&t, json!({ "op": "branch-create", "name": "bad name?" })).unwrap_err();
    assert!(err.to_string().contains("not a valid branch name"), "{err}");

    op(&t, json!({ "op": "branch-rename", "name": "topic", "to": "feature/topic" })).unwrap();
    assert_eq!(branches(&t), vec!["feature/topic", "main"]);
    assert!(op(&t, json!({ "op": "branch-rename", "name": "feature/topic", "to": "main" })).is_err());

    // The branch points to a commit that main also has: it is deleted without questions.
    let deleted = op(&t, json!({ "op": "branch-delete", "name": "feature/topic" })).unwrap();
    assert_eq!(deleted["deleted"], "feature/topic");
    assert_eq!(branches(&t), vec!["main"]);

    let err = op(&t, json!({ "op": "branch-delete", "name": "main" })).unwrap_err();
    assert!(err.to_string().contains("current branch"), "{err}");
    let _ = b;
}

#[test]
fn deleting_a_branch_with_its_own_commits_needs_force_and_can_be_undone() {
    let t = TestRepo::new();
    let base = t.commit_file("a.txt", "1", "base");
    let w1 = t.raw_commit("work one", &[base]);
    let w2 = t.raw_commit("work two", &[w1]);
    t.branch("work", w2);

    let asked = op(&t, json!({ "op": "branch-delete", "name": "work" })).unwrap();
    assert_eq!(asked["needsForce"], true);
    let summaries: Vec<&str> = asked["commits"].as_array().unwrap().iter().map(|c| c["summary"].as_str().unwrap()).collect();
    assert_eq!(summaries, vec!["work two", "work one"], "the commits that would be lost are listed");
    assert_eq!(branches(&t), vec!["main", "work"], "nothing is deleted without force");

    let deleted = op(&t, json!({ "op": "branch-delete", "name": "work", "force": true })).unwrap();
    assert_eq!(deleted["oid"], w2.to_string());
    assert_eq!(branches(&t), vec!["main"]);

    op(&t, json!({ "op": "branch-restore", "name": "work", "oid": w2.to_string() })).unwrap();
    assert_eq!(branches(&t), vec!["main", "work"]);

    // A tag on the tip keeps the commits reachable, so no force is needed.
    t.tag("keep", w2, false);
    let deleted = op(&t, json!({ "op": "branch-delete", "name": "work" })).unwrap();
    assert_eq!(deleted["deleted"], "work");
}

#[test]
fn tags_are_created_and_deleted() {
    let t = TestRepo::new();
    let a = t.commit_file("a.txt", "1", "first");
    let b = t.commit_file("a.txt", "2", "second");
    op(&t, json!({ "op": "tag-create", "name": "v1", "at": a.to_string() })).unwrap();
    op(&t, json!({ "op": "tag-create", "name": "v2", "message": "Release two" })).unwrap();
    let refs = repo::refs(&repo::open(&t.root()).unwrap()).unwrap();
    let tags: Vec<(String, String, bool)> = refs.tags.iter().map(|t| (t.name.clone(), t.oid.clone(), t.annotated)).collect();
    assert_eq!(tags, vec![("v2".to_string(), b.to_string(), true), ("v1".to_string(), a.to_string(), false)]);

    assert!(op(&t, json!({ "op": "tag-create", "name": "v1" })).unwrap_err().to_string().contains("already exists"));
    assert!(op(&t, json!({ "op": "tag-create", "name": "bad..name" })).is_err());
    op(&t, json!({ "op": "tag-delete", "name": "v1" })).unwrap();
    assert_eq!(repo::refs(&repo::open(&t.root()).unwrap()).unwrap().tags.len(), 1);
    assert!(op(&t, json!({ "op": "tag-delete", "name": "v1" })).is_err());
}

#[test]
fn remotes_are_added_renamed_and_removed() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "1", "first");
    op(&t, json!({ "op": "remote-add", "name": "origin", "url": "https://example.com/a.git" })).unwrap();
    assert!(op(&t, json!({ "op": "remote-add", "name": "origin", "url": "https://example.com/b.git" }))
        .unwrap_err()
        .to_string()
        .contains("already exists"));
    assert!(op(&t, json!({ "op": "remote-add", "name": "empty", "url": "  " })).is_err());
    op(&t, json!({ "op": "remote-set-url", "name": "origin", "url": "https://example.com/moved.git" })).unwrap();
    op(&t, json!({ "op": "remote-rename", "name": "origin", "to": "upstream" })).unwrap();
    let refs = repo::refs(&repo::open(&t.root()).unwrap()).unwrap();
    assert_eq!(refs.remotes.len(), 1);
    assert_eq!(refs.remotes[0].name, "upstream");
    assert_eq!(refs.remotes[0].url.as_deref(), Some("https://example.com/moved.git"));
    op(&t, json!({ "op": "remote-remove", "name": "upstream" })).unwrap();
    assert!(repo::refs(&repo::open(&t.root()).unwrap()).unwrap().remotes.is_empty());
    assert!(op(&t, json!({ "op": "remote-remove", "name": "upstream" })).is_err());
}

#[test]
fn checkout_switches_branches_and_refuses_to_overwrite_local_changes() {
    if !has_git() {
        return;
    }
    let t = TestRepo::new();
    let a = t.commit_file("a.txt", "one\n", "first");
    t.commit_file("a.txt", "two\n", "second");
    op(&t, json!({ "op": "branch-create", "name": "old", "at": a.to_string(), "checkout": true })).unwrap();
    assert_eq!(head_branch(&t).as_deref(), Some("old"));
    assert_eq!(t.read("a.txt"), "one\n");

    op(&t, json!({ "op": "checkout", "target": "main", "kind": "branch" })).unwrap();
    assert_eq!(t.read("a.txt"), "two\n");

    // A local change that the checkout would overwrite: git refuses, and nothing is lost.
    t.write("a.txt", "my unsaved work\n");
    let err = op(&t, json!({ "op": "checkout", "target": "old", "kind": "branch" })).unwrap_err();
    let git = err.downcast_ref::<GitError>().expect("the git error is passed on unchanged");
    assert_eq!(git.command, "git checkout old");
    assert!(git.output.contains("would be overwritten"), "{}", git.output);
    assert_eq!(t.read("a.txt"), "my unsaved work\n");
    assert_eq!(head_branch(&t).as_deref(), Some("main"));
    worktree::discard(&repo::open(&t.root()).unwrap(), &["a.txt".into()]).unwrap();

    // A commit id gives a detached HEAD.
    op(&t, json!({ "op": "checkout", "target": a.to_string(), "kind": "commit" })).unwrap();
    let head = repo::head(&repo::open(&t.root()).unwrap());
    assert!(head.detached);
    assert_eq!(head.oid.as_deref(), Some(a.to_string().as_str()));
}

#[test]
fn checkout_of_a_remote_branch_creates_a_tracking_branch() {
    if !has_git() {
        return;
    }
    let t = TestRepo::new();
    let a = t.commit_file("a.txt", "1", "first");
    let b = t.raw_commit("remote work", &[a]);
    t.repo.remote("origin", "https://example.com/demo.git").unwrap();
    t.set_ref("refs/remotes/origin/feature/x", b);

    let res = op(&t, json!({ "op": "checkout", "target": "origin/feature/x", "kind": "remote" })).unwrap();
    assert_eq!(res["branch"], "feature/x");
    assert_eq!(head_branch(&t).as_deref(), Some("feature/x"));
    let refs = repo::refs(&repo::open(&t.root()).unwrap()).unwrap();
    let local = refs.branches.iter().find(|b| b.name == "feature/x").unwrap();
    assert_eq!(local.upstream.as_deref(), Some("origin/feature/x"));
    assert_eq!(local.oid, b.to_string());

    // The second time the existing local branch is checked out.
    op(&t, json!({ "op": "checkout", "target": "main", "kind": "branch" })).unwrap();
    op(&t, json!({ "op": "checkout", "target": "origin/feature/x", "kind": "remote" })).unwrap();
    assert_eq!(head_branch(&t).as_deref(), Some("feature/x"));
    assert!(op(&t, json!({ "op": "checkout", "target": "nowhere/x", "kind": "remote" })).is_err());
}

#[test]
fn stash_save_apply_pop_drop_and_restore() {
    if !has_git() {
        return;
    }
    let t = TestRepo::new();
    t.commit_file("a.txt", "one\n", "first");
    let stash_count = |t: &TestRepo| repo::refs(&repo::open(&t.root()).unwrap()).unwrap().stashes.len();

    assert!(op(&t, json!({ "op": "stash-save" })).unwrap_err().to_string().contains("no local changes"));

    t.write("a.txt", "changed\n");
    t.write("new.txt", "untracked\n");
    op(&t, json!({ "op": "stash-save", "message": "first try", "untracked": true })).unwrap();
    assert_eq!(t.read("a.txt"), "one\n");
    assert!(!t.path().join("new.txt").exists(), "untracked files go into the stash when asked");
    assert_eq!(stash_count(&t), 1);
    assert!(repo::refs(&repo::open(&t.root()).unwrap()).unwrap().stashes[0].message.contains("first try"));

    // Apply keeps the stash, pop removes it.
    op(&t, json!({ "op": "stash-apply", "index": 0 })).unwrap();
    assert_eq!(t.read("a.txt"), "changed\n");
    assert_eq!(t.read("new.txt"), "untracked\n");
    assert_eq!(stash_count(&t), 1);
    worktree::discard(&repo::open(&t.root()).unwrap(), &["a.txt".into(), "new.txt".into()]).unwrap();
    op(&t, json!({ "op": "stash-pop", "index": 0 })).unwrap();
    assert_eq!(t.read("a.txt"), "changed\n");
    assert_eq!(stash_count(&t), 0);

    // Drop returns what is needed to put the stash back.
    op(&t, json!({ "op": "stash-save", "message": "second try", "untracked": true })).unwrap();
    let dropped = op(&t, json!({ "op": "stash-drop", "index": 0 })).unwrap();
    assert_eq!(stash_count(&t), 0);
    assert!(op(&t, json!({ "op": "stash-drop", "index": 0 })).is_err());
    op(&t, json!({ "op": "stash-store", "oid": dropped["oid"], "message": dropped["message"] })).unwrap();
    assert_eq!(stash_count(&t), 1);
    op(&t, json!({ "op": "stash-pop", "index": 0 })).unwrap();
    assert_eq!(t.read("a.txt"), "changed\n");
    assert_eq!(t.read("new.txt"), "untracked\n");
}

// ---------------------------------------------------------------- network jobs

/// Runs a job and waits for its final state.
fn run_job(dir: &Path, title: &str, args: Vec<String>) -> JobInfo {
    let (tx, mut rx) = tokio::sync::broadcast::channel(4096);
    let jobs = Jobs::new(tx);
    let started = jobs.start(None, title, dir.to_path_buf(), args, |_| {}).expect("the job starts");
    assert_eq!(started.state, "running");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        assert!(std::time::Instant::now() < deadline, "the job did not finish");
        match rx.try_recv() {
            Ok(text) => {
                let v: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(v["type"], "job");
                if v["job"]["state"] != "running" {
                    assert!(jobs.list().is_empty(), "a finished job leaves the list");
                    return JobInfo {
                        id: v["job"]["id"].as_str().unwrap().into(),
                        repo: None,
                        title: v["job"]["title"].as_str().unwrap().into(),
                        command: v["job"]["command"].as_str().unwrap().into(),
                        line: v["job"]["line"].as_str().unwrap().into(),
                        percent: None,
                        state: match v["job"]["state"].as_str().unwrap() {
                            "done" => "done",
                            "cancelled" => "cancelled",
                            _ => "failed",
                        },
                        output: v["job"]["output"].as_str().map(str::to_string),
                    };
                }
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
}

fn net(t: &TestRepo, job: Value) -> JobInfo {
    let job: NetJob = serde_json::from_value(job).expect("a valid job");
    let (title, args) = job.command();
    run_job(&t.root(), &title, args)
}

/// A bare repository that serves as the remote, as a path git accepts as URL.
fn bare_remote() -> (tempfile::TempDir, String) {
    common::isolate_git_config();
    let dir = tempfile::tempdir().unwrap();
    // The remote's HEAD must name the branch the tests push, or a clone checks out nothing.
    let mut opts = git2::RepositoryInitOptions::new();
    opts.bare(true).initial_head("main");
    git2::Repository::init_opts(dir.path(), &opts).unwrap();
    let url = dir.path().display().to_string().replace('\\', "/");
    (dir, url)
}

#[test]
fn push_fetch_and_pull_through_a_local_remote() {
    if !has_git() {
        return;
    }
    let (_remote_dir, url) = bare_remote();
    let alice = TestRepo::new();
    let a1 = alice.commit_file("a.txt", "one\n", "first");
    op(&alice, json!({ "op": "remote-add", "name": "origin", "url": url })).unwrap();

    let pushed = net(&alice, json!({ "job": "push", "remote": "origin", "refname": "main", "setUpstream": true }));
    assert_eq!(pushed.state, "done", "{:?}", pushed.output);
    assert_eq!(pushed.command, "git push --progress --set-upstream origin main");
    let refs = repo::refs(&repo::open(&alice.root()).unwrap()).unwrap();
    assert_eq!(refs.branches[0].upstream.as_deref(), Some("origin/main"));
    assert_eq!((refs.branches[0].ahead, refs.branches[0].behind), (0, 0));

    // Bob clones, commits and pushes.
    let bob_parent = tempfile::tempdir().unwrap();
    let cloned = run_job(bob_parent.path(), "Clone", vec!["clone".into(), "--progress".into(), url.clone(), "bob".into()]);
    assert_eq!(cloned.state, "done", "{:?}", cloned.output);
    let bob_path = bob_parent.path().join("bob");
    let bob = git2::Repository::open(&bob_path).unwrap();
    assert_eq!(bob.head().unwrap().peel_to_commit().unwrap().id(), a1);
    std::fs::write(bob_path.join("a.txt"), "one\ntwo\n").unwrap();
    gitcli::run(&bob_path, &["-c", "user.name=Bob", "-c", "user.email=bob@example.com", "commit", "-am", "second"], None).unwrap();
    gitcli::run(&bob_path, &["push", "origin", "HEAD"], None).unwrap();
    let b2 = bob.head().unwrap().peel_to_commit().unwrap().id();

    // Alice fetches: she is one commit behind, her files are unchanged.
    let fetched = net(&alice, json!({ "job": "fetch" }));
    assert_eq!(fetched.state, "done", "{:?}", fetched.output);
    let refs = repo::refs(&repo::open(&alice.root()).unwrap()).unwrap();
    assert_eq!((refs.branches[0].ahead, refs.branches[0].behind), (0, 1));
    assert_eq!(alice.read("a.txt"), "one\n");

    let pulled = net(&alice, json!({ "job": "pull", "mode": "ff-only" }));
    assert_eq!(pulled.state, "done", "{:?}", pulled.output);
    assert_eq!(alice.read("a.txt"), "one\ntwo\n");
    assert_eq!(repo::open(&alice.root()).unwrap().head().unwrap().peel_to_commit().unwrap().id(), b2);
}

#[test]
fn force_push_with_lease_does_not_overwrite_commits_it_has_not_seen() {
    if !has_git() {
        return;
    }
    let (_remote_dir, url) = bare_remote();
    let alice = TestRepo::new();
    alice.commit_file("a.txt", "one\n", "first");
    op(&alice, json!({ "op": "remote-add", "name": "origin", "url": url })).unwrap();
    assert_eq!(net(&alice, json!({ "job": "push", "remote": "origin", "refname": "main", "setUpstream": true })).state, "done");

    // Someone else pushes a commit that Alice has not fetched.
    let other_parent = tempfile::tempdir().unwrap();
    gitcli::run(other_parent.path(), &["clone", &url, "other"], None).unwrap();
    let other = other_parent.path().join("other");
    std::fs::write(other.join("theirs.txt"), "their work\n").unwrap();
    gitcli::run(&other, &["add", "."], None).unwrap();
    gitcli::run(&other, &["-c", "user.name=Other", "-c", "user.email=o@example.com", "commit", "-m", "their commit"], None).unwrap();
    gitcli::run(&other, &["push", "origin", "HEAD"], None).unwrap();
    let theirs = git2::Repository::open(&other).unwrap().head().unwrap().peel_to_commit().unwrap().id();

    // Alice rewrites her commit and pushes with force.
    let repo_alice = repo::open(&alice.root()).unwrap();
    alice.write("a.txt", "rewritten\n");
    worktree::stage(&repo_alice, &["a.txt".into()]).unwrap();
    worktree::commit(&repo_alice, &alice.root(), true, &worktree::CommitRequest { message: "first, rewritten".into(), amend: true })
        .unwrap();

    // A plain push is rejected.
    let plain = net(&alice, json!({ "job": "push", "remote": "origin", "refname": "main" }));
    assert_eq!(plain.state, "failed");
    assert!(plain.output.as_deref().unwrap_or("").contains("rejected"), "{:?}", plain.output);

    // The forced push uses the lease: the remote has a commit Alice never saw, so it is refused.
    let forced = net(&alice, json!({ "job": "push", "remote": "origin", "refname": "main", "force": true }));
    assert_eq!(forced.command, "git push --progress --force-with-lease origin main");
    assert_eq!(forced.state, "failed", "the lease must protect the other commit");
    assert!(forced.output.as_deref().unwrap_or("").contains("stale info"), "{:?}", forced.output);
    let remote = git2::Repository::open(_remote_dir.path()).unwrap();
    assert_eq!(remote.find_reference("refs/heads/main").unwrap().target().unwrap(), theirs, "their commit is still on the remote");

    // After a fetch Alice has seen it; a forced push is then her decision and goes through.
    assert_eq!(net(&alice, json!({ "job": "fetch" })).state, "done");
    let forced = net(&alice, json!({ "job": "push", "remote": "origin", "refname": "main", "force": true }));
    assert_eq!(forced.state, "done", "{:?}", forced.output);
}

#[test]
fn a_failed_job_reports_the_command_and_the_output() {
    if !has_git() {
        return;
    }
    let parent = tempfile::tempdir().unwrap();
    let missing = parent.path().join("no-such-remote").display().to_string().replace('\\', "/");
    let job = run_job(parent.path(), "Clone", vec!["clone".into(), "--progress".into(), missing, "target".into()]);
    assert_eq!(job.state, "failed");
    assert!(job.command.starts_with("git clone --progress "));
    // The wording of git differs between versions and systems; both name the missing repository.
    let output = job.output.as_deref().unwrap_or("");
    assert!(output.contains("does not appear to be a git repository") || output.contains("does not exist"), "{output}");
    assert!(output.contains("no-such-remote"), "{output}");
    assert!(!parent.path().join("target").exists());
}

#[test]
fn deleting_a_remote_branch_goes_through_push() {
    if !has_git() {
        return;
    }
    let (remote_dir, url) = bare_remote();
    let t = TestRepo::new();
    let a = t.commit_file("a.txt", "one\n", "first");
    t.branch("topic", a);
    op(&t, json!({ "op": "remote-add", "name": "origin", "url": url })).unwrap();
    assert_eq!(net(&t, json!({ "job": "push", "remote": "origin", "refname": "topic" })).state, "done");
    let remote = git2::Repository::open(remote_dir.path()).unwrap();
    assert!(remote.find_reference("refs/heads/topic").is_ok());
    let deleted = net(&t, json!({ "job": "push-delete", "remote": "origin", "refname": "topic" }));
    assert_eq!(deleted.state, "done", "{:?}", deleted.output);
    assert!(remote.find_reference("refs/heads/topic").is_err());
}

#[test]
fn a_running_job_can_be_cancelled() {
    if !has_git() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (tx, mut rx) = tokio::sync::broadcast::channel(256);
    let jobs = Jobs::new(tx);
    // A git command that would run for a minute: an alias that starts a shell and sleeps.
    let args = vec!["-c".to_string(), "alias.wait=!sleep 60".to_string(), "wait".to_string()];
    let started = std::time::Instant::now();
    let job = jobs.start(None, "Wait", dir.path().to_path_buf(), args, |_| {}).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert_eq!(jobs.list().len(), 1, "the job is running");
    assert!(!jobs.cancel("no-such-job"));
    assert!(jobs.cancel(&job.id));
    let mut last = String::new();
    while started.elapsed() < std::time::Duration::from_secs(20) {
        match rx.try_recv() {
            Ok(text) => {
                let v: Value = serde_json::from_str(&text).unwrap();
                last = v["job"]["state"].as_str().unwrap().to_string();
                if last != "running" {
                    break;
                }
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
    assert_eq!(last, "cancelled", "the job must stop long before the minute is over");
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert!(jobs.list().is_empty());
}
