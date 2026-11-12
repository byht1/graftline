//! Operations that change history, and conflict resolution.

mod common;

use common::TestRepo;
use git2::Oid;
use graftline::conflicts::{self, Choice};
use graftline::ops::{self, Op};
use graftline::rewrite::{self, HistoryOp};
use graftline::{gitcli, repo, safety, worktree};
use serde_json::{json, Value};

fn has_git() -> bool {
    // git starts this binary as the sequence editor of an interactive rebase.
    std::env::set_var("GRAFTLINE_EDITOR_EXE", env!("CARGO_BIN_EXE_graftline-server"));
    let found = gitcli::detect().is_some();
    if !found {
        eprintln!("skipped: git is not on PATH");
    }
    found
}

fn op(t: &TestRepo, value: Value) -> anyhow::Result<Value> {
    let op: Op = serde_json::from_value(value).expect("a valid operation");
    ops::run(&repo::open(&t.root())?, &t.root(), op)
}

fn hist(t: &TestRepo, value: Value) -> anyhow::Result<Value> {
    let op: HistoryOp = serde_json::from_value(value).expect("a valid history operation");
    rewrite::run(&repo::open(&t.root())?, &t.root(), op)
}

fn fresh(t: &TestRepo) -> git2::Repository {
    repo::open(&t.root()).unwrap()
}

fn head(t: &TestRepo) -> Oid {
    fresh(t).head().unwrap().peel_to_commit().unwrap().id()
}

/// Summaries from HEAD down the first-parent chain.
fn log(t: &TestRepo) -> Vec<String> {
    let r = fresh(t);
    let mut out = Vec::new();
    let mut c = r.head().unwrap().peel_to_commit().ok();
    while let Some(commit) = c {
        out.push(commit.summary().unwrap().unwrap_or("").to_string());
        c = commit.parent(0).ok();
    }
    out
}

fn state(t: &TestRepo) -> Option<&'static str> {
    repo::state_name(fresh(t).state())
}

fn conflict_paths(t: &TestRepo) -> Vec<String> {
    conflicts::list(&fresh(t)).unwrap().into_iter().map(|c| c.path).collect()
}

/// main and `topic` both change line 2 of f.txt; `topic` also adds its own file.
fn diverged() -> (TestRepo, Oid, Oid) {
    let t = TestRepo::new();
    t.commit_file("f.txt", "one\ntwo\nthree\n", "base");
    op(&t, json!({ "op": "branch-create", "name": "topic", "checkout": true })).unwrap();
    t.write("f.txt", "one\nTWO from topic\nthree\n");
    t.write("topic.txt", "only on topic\n");
    t.stage_all();
    let topic = t.commit("topic changes line two");
    op(&t, json!({ "op": "checkout", "target": "main", "kind": "branch" })).unwrap();
    let main = t.commit_file("f.txt", "one\nTWO from main\nthree\n", "main changes line two");
    (t, main, topic)
}

#[test]
fn merge_without_conflicts_creates_a_merge_commit_or_fast_forwards() {
    if !has_git() {
        return;
    }
    let t = TestRepo::new();
    let base = t.commit_file("a.txt", "a\n", "base");
    op(&t, json!({ "op": "branch-create", "name": "topic", "checkout": true })).unwrap();
    let topic = t.commit_file("b.txt", "b\n", "topic work");
    op(&t, json!({ "op": "checkout", "target": "main", "kind": "branch" })).unwrap();

    // main has no commits of its own: the merge moves it forward.
    let res = op(&t, json!({ "op": "merge", "branch": "topic" })).unwrap();
    assert_eq!(res["before"], base.to_string());
    assert!(res.get("stopped").is_none());
    assert_eq!(head(&t), topic);

    // With commits on both sides a merge commit appears; `into` checks out the target first.
    op(&t, json!({ "op": "checkout", "target": "topic", "kind": "branch" })).unwrap();
    t.commit_file("c.txt", "c\n", "more topic work");
    op(&t, json!({ "op": "checkout", "target": "main", "kind": "branch" })).unwrap();
    t.commit_file("d.txt", "d\n", "main work");
    op(&t, json!({ "op": "checkout", "target": "topic", "kind": "branch" })).unwrap();
    op(&t, json!({ "op": "merge", "branch": "topic", "into": "main" })).unwrap();
    assert_eq!(repo::head(&fresh(&t)).branch.as_deref(), Some("main"));
    let merge = fresh(&t).find_commit(head(&t)).unwrap().parent_count();
    assert_eq!(merge, 2);
    assert!(t.path().join("c.txt").exists() && t.path().join("d.txt").exists());
}

#[test]
fn merge_conflict_is_resolved_by_edited_content_and_continued() {
    if !has_git() {
        return;
    }
    let (t, main, topic) = diverged();
    let res = op(&t, json!({ "op": "merge", "branch": "topic" })).unwrap();
    assert_eq!(res["stopped"], "merge", "{res}");
    assert_eq!(res["before"], main.to_string());
    assert!(res["output"].as_str().unwrap().contains("CONFLICT"), "{res}");
    assert_eq!(state(&t), Some("merge"));
    assert_eq!(conflict_paths(&t), vec!["f.txt"]);

    let c = &conflicts::list(&fresh(&t)).unwrap()[0];
    assert_eq!(c.kind, "both-modified");
    let d = conflicts::detail(&fresh(&t), "f.txt").unwrap();
    assert_eq!(d.base.as_deref(), Some("one\ntwo\nthree\n"));
    assert_eq!(d.ours.as_deref(), Some("one\nTWO from main\nthree\n"));
    assert_eq!(d.theirs.as_deref(), Some("one\nTWO from topic\nthree\n"));
    let merged = d.merged.unwrap();
    assert!(merged.contains("<<<<<<<") && merged.contains("=======") && merged.contains(">>>>>>>"));
    assert!(!d.binary);
    assert!(d.ours_label.contains("main"), "{}", d.ours_label);
    assert!(d.theirs_label.contains(&topic.to_string()[..7]), "{}", d.theirs_label);

    // Continuing is refused while a conflict is left.
    let err = hist(&t, json!({ "op": "continue" })).unwrap_err();
    assert!(err.to_string().contains("Resolve the conflicts first: f.txt"), "{err}");

    let snapshot = conflicts::resolve(&fresh(&t), "f.txt", Choice::Content { content: "one\nTWO from both\nthree\n".into() }).unwrap();
    assert!(snapshot.is_some(), "the file with markers is saved before it is replaced");
    assert!(conflict_paths(&t).is_empty());
    assert_eq!(t.read("f.txt"), "one\nTWO from both\nthree\n");

    hist(&t, json!({ "op": "continue" })).unwrap();
    assert_eq!(state(&t), None);
    let after = fresh(&t);
    let merge = after.find_commit(head(&t)).unwrap();
    assert_eq!(merge.parent_count(), 2);
    assert_eq!(merge.parent_id(0).unwrap(), main);
    assert_eq!(merge.parent_id(1).unwrap(), topic);
    assert_eq!(t.read("topic.txt"), "only on topic\n");
    assert!(!worktree::is_dirty(&fresh(&t)));
}

#[test]
fn conflict_is_resolved_by_taking_a_whole_side() {
    if !has_git() {
        return;
    }
    for (choice, expected) in [("ours", "one\nTWO from main\nthree\n"), ("theirs", "one\nTWO from topic\nthree\n")] {
        let (t, _, _) = diverged();
        op(&t, json!({ "op": "merge", "branch": "topic" })).unwrap();
        let choice = if choice == "ours" { Choice::Ours } else { Choice::Theirs };
        conflicts::resolve(&fresh(&t), "f.txt", choice).unwrap();
        assert_eq!(t.read("f.txt"), expected);
        assert!(conflict_paths(&t).is_empty());
        hist(&t, json!({ "op": "continue" })).unwrap();
        assert_eq!(state(&t), None);
        assert_eq!(t.read("f.txt"), expected);
    }
}

#[test]
fn resolved_file_can_get_its_conflict_markers_back() {
    if !has_git() {
        return;
    }
    let (t, _, _) = diverged();
    op(&t, json!({ "op": "merge", "branch": "topic" })).unwrap();
    conflicts::resolve(&fresh(&t), "f.txt", Choice::Ours).unwrap();
    assert!(conflict_paths(&t).is_empty());
    hist(&t, json!({ "op": "unresolve", "path": "f.txt" })).unwrap();
    assert_eq!(conflict_paths(&t), vec!["f.txt"]);
    assert!(t.read("f.txt").contains("<<<<<<<"));
}

#[test]
fn delete_and_modify_conflict_keeps_or_deletes_the_file() {
    if !has_git() {
        return;
    }
    let setup = || {
        let t = TestRepo::new();
        t.commit_file("f.txt", "one\n", "base");
        op(&t, json!({ "op": "branch-create", "name": "topic", "checkout": true })).unwrap();
        t.commit_file("f.txt", "one\nchanged on topic\n", "topic edits the file");
        op(&t, json!({ "op": "checkout", "target": "main", "kind": "branch" })).unwrap();
        t.remove("f.txt");
        t.stage_all();
        t.commit("main deletes the file");
        let res = op(&t, json!({ "op": "merge", "branch": "topic" })).unwrap();
        assert_eq!(res["stopped"], "merge");
        t
    };
    let t = setup();
    let c = &conflicts::list(&fresh(&t)).unwrap()[0];
    assert_eq!((c.kind, c.ours, c.theirs), ("deleted-by-us", false, true));
    // Taking their side keeps the edited file.
    conflicts::resolve(&fresh(&t), "f.txt", Choice::Theirs).unwrap();
    hist(&t, json!({ "op": "continue" })).unwrap();
    assert_eq!(t.read("f.txt"), "one\nchanged on topic\n");

    // Taking our side keeps it deleted.
    let t = setup();
    conflicts::resolve(&fresh(&t), "f.txt", Choice::Ours).unwrap();
    assert!(conflict_paths(&t).is_empty());
    hist(&t, json!({ "op": "continue" })).unwrap();
    assert!(!t.path().join("f.txt").exists());
    assert_eq!(state(&t), None);
}

#[test]
fn abort_returns_to_the_state_before_the_merge_and_keeps_a_snapshot() {
    if !has_git() {
        return;
    }
    let (t, main, _) = diverged();
    op(&t, json!({ "op": "merge", "branch": "topic" })).unwrap();
    t.write("f.txt", "one\nhalf finished manual merge\nthree\n");
    let res = hist(&t, json!({ "op": "abort" })).unwrap();
    assert_eq!(state(&t), None);
    assert_eq!(head(&t), main);
    assert_eq!(t.read("f.txt"), "one\nTWO from main\nthree\n");
    // The half-finished resolution is not lost.
    let name = res["snapshot"]["name"].as_str().expect("the abort saved a snapshot");
    safety::restore(&fresh(&t), name).unwrap();
    assert_eq!(t.read("f.txt"), "one\nhalf finished manual merge\nthree\n");
    assert!(hist(&t, json!({ "op": "abort" })).unwrap_err().to_string().contains("No operation is in progress"));
}

#[test]
fn rebase_replays_commits_and_stops_for_conflicts() {
    if !has_git() {
        return;
    }
    // Without conflicts.
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "base");
    op(&t, json!({ "op": "branch-create", "name": "topic", "checkout": true })).unwrap();
    t.commit_file("t1.txt", "1\n", "topic one");
    let old_tip = t.commit_file("t2.txt", "2\n", "topic two");
    op(&t, json!({ "op": "checkout", "target": "main", "kind": "branch" })).unwrap();
    let main = t.commit_file("m.txt", "m\n", "main work");
    // The branch is named, so it is checked out first.
    let res = op(&t, json!({ "op": "rebase", "branch": "topic", "onto": "main" })).unwrap();
    assert_eq!(res["before"], old_tip.to_string());
    assert!(res.get("stopped").is_none(), "{res}");
    assert_eq!(repo::head(&fresh(&t)).branch.as_deref(), Some("topic"));
    assert_eq!(log(&t), vec!["topic two", "topic one", "main work", "base"]);
    assert_ne!(head(&t), old_tip, "the commits are rewritten");
    assert_eq!(fresh(&t).find_commit(head(&t)).unwrap().parent(0).unwrap().parent_id(0).unwrap(), main);

    // With a conflict: stop, resolve, continue.
    let (t, main, _) = diverged();
    let res = op(&t, json!({ "op": "rebase", "branch": "topic", "onto": "main" })).unwrap();
    assert_eq!(res["stopped"], "rebase", "{res}");
    assert_eq!(conflict_paths(&t), vec!["f.txt"]);
    let d = conflicts::detail(&fresh(&t), "f.txt").unwrap();
    // In a rebase "ours" is the new base and "theirs" is the user's commit: the labels say so.
    assert_eq!(d.ours.as_deref(), Some("one\nTWO from main\nthree\n"));
    assert!(d.ours_label.contains("new base"), "{}", d.ours_label);
    assert!(d.theirs_label.contains("being rebased"), "{}", d.theirs_label);
    conflicts::resolve(&fresh(&t), "f.txt", Choice::Theirs).unwrap();
    hist(&t, json!({ "op": "continue" })).unwrap();
    assert_eq!(state(&t), None);
    assert_eq!(log(&t), vec!["topic changes line two", "main changes line two", "base"]);
    assert_eq!(t.read("f.txt"), "one\nTWO from topic\nthree\n");
    assert_eq!(fresh(&t).find_commit(head(&t)).unwrap().parent_id(0).unwrap(), main);
}

#[test]
fn rebase_can_be_skipped_or_aborted() {
    if !has_git() {
        return;
    }
    let (t, _, topic) = diverged();
    op(&t, json!({ "op": "rebase", "branch": "topic", "onto": "main" })).unwrap();
    hist(&t, json!({ "op": "abort" })).unwrap();
    assert_eq!(state(&t), None);
    assert_eq!(head(&t), topic, "after an abort the branch is where it was");

    op(&t, json!({ "op": "rebase", "branch": "topic", "onto": "main" })).unwrap();
    hist(&t, json!({ "op": "skip" })).unwrap();
    assert_eq!(state(&t), None);
    assert_eq!(log(&t), vec!["main changes line two", "base"], "the conflicting commit is left out");
}

#[test]
fn rebase_info_counts_commits_that_are_already_published() {
    let t = TestRepo::new();
    let base = t.commit_file("a.txt", "a\n", "base");
    let c1 = t.raw_commit("pushed one", &[base]);
    let c2 = t.raw_commit("pushed two", &[c1]);
    let c3 = t.raw_commit("local only", &[c2]);
    let other = t.raw_commit("main moved on", &[base]);
    t.branch("topic", c3);
    t.set_ref("refs/heads/main", other);
    t.repo.remote("origin", "https://example.com/x.git").unwrap();
    t.set_ref("refs/remotes/origin/topic", c2);
    t.repo.find_branch("topic", git2::BranchType::Local).unwrap().set_upstream(Some("origin/topic")).unwrap();
    let info = rewrite::rebase_info(&fresh(&t), Some("topic"), "main").unwrap();
    assert_eq!(info, json!({ "commits": 3, "published": 2 }));
    let info = rewrite::rebase_info(&fresh(&t), Some("main"), "topic").unwrap();
    assert_eq!(info, json!({ "commits": 1, "published": 0 }));
}

#[test]
fn cherry_pick_applies_commits_and_handles_conflicts() {
    if !has_git() {
        return;
    }
    let t = TestRepo::new();
    t.commit_file("a.txt", "a\n", "base");
    op(&t, json!({ "op": "branch-create", "name": "topic", "checkout": true })).unwrap();
    let p1 = t.commit_file("p1.txt", "1\n", "pick one");
    let p2 = t.commit_file("p2.txt", "2\n", "pick two");
    op(&t, json!({ "op": "checkout", "target": "main", "kind": "branch" })).unwrap();
    let before = head(&t);
    let res = hist(&t, json!({ "op": "cherry-pick", "oids": [p1.to_string(), p2.to_string()] })).unwrap();
    assert_eq!(res["before"], before.to_string());
    assert_eq!(log(&t), vec!["pick two", "pick one", "base"]);
    assert!(t.path().join("p1.txt").exists() && t.path().join("p2.txt").exists());
    assert!(hist(&t, json!({ "op": "cherry-pick", "oids": [] })).is_err());

    let (t, _, topic) = diverged();
    let res = hist(&t, json!({ "op": "cherry-pick", "oids": [topic.to_string()] })).unwrap();
    assert_eq!(res["stopped"], "cherry-pick", "{res}");
    conflicts::resolve(&fresh(&t), "f.txt", Choice::Theirs).unwrap();
    hist(&t, json!({ "op": "continue" })).unwrap();
    assert_eq!(state(&t), None);
    assert_eq!(log(&t)[0], "topic changes line two");
    assert_eq!(t.read("f.txt"), "one\nTWO from topic\nthree\n");
}

#[test]
fn revert_adds_a_commit_that_undoes_one() {
    if !has_git() {
        return;
    }
    let t = TestRepo::new();
    t.commit_file("a.txt", "one\n", "base");
    let bad = t.commit_file("a.txt", "one\nmistake\n", "add a mistake");
    t.commit_file("b.txt", "b\n", "later work");
    hist(&t, json!({ "op": "revert", "oid": bad.to_string() })).unwrap();
    assert_eq!(log(&t), vec!["Revert \"add a mistake\"", "later work", "add a mistake", "base"]);
    assert_eq!(t.read("a.txt"), "one\n");
    assert_eq!(t.read("b.txt"), "b\n");

    // Reverting a merge commit undoes what the merged branch brought in.
    let (t, _, _) = diverged();
    op(&t, json!({ "op": "merge", "branch": "topic" })).unwrap();
    conflicts::resolve(&fresh(&t), "f.txt", Choice::Theirs).unwrap();
    hist(&t, json!({ "op": "continue" })).unwrap();
    let merge = head(&t);
    hist(&t, json!({ "op": "revert", "oid": merge.to_string() })).unwrap();
    assert_eq!(t.read("f.txt"), "one\nTWO from main\nthree\n");
    assert!(!t.path().join("topic.txt").exists());
}

#[test]
fn reset_soft_mixed_and_hard() {
    if !has_git() {
        return;
    }
    let setup = || {
        let t = TestRepo::new();
        let a = t.commit_file("a.txt", "one\n", "first");
        let b = t.commit_file("a.txt", "one\ntwo\n", "second");
        (t, a, b)
    };
    let staged = |t: &TestRepo| worktree::status(&fresh(t)).unwrap().staged.len();
    let unstaged = |t: &TestRepo| worktree::status(&fresh(t)).unwrap().unstaged.len();

    let (t, a, b) = setup();
    let res = hist(&t, json!({ "op": "reset", "to": a.to_string(), "mode": "soft" })).unwrap();
    assert_eq!(res["before"], b.to_string());
    assert_eq!((head(&t), staged(&t), unstaged(&t)), (a, 1, 0), "soft: the change of the removed commit is staged");
    assert_eq!(t.read("a.txt"), "one\ntwo\n");

    let (t, a, _) = setup();
    hist(&t, json!({ "op": "reset", "to": a.to_string(), "mode": "mixed" })).unwrap();
    assert_eq!((head(&t), staged(&t), unstaged(&t)), (a, 0, 1), "mixed: the change is in the working tree only");
    assert_eq!(t.read("a.txt"), "one\ntwo\n");

    let (t, a, b) = setup();
    t.write("a.txt", "one\ntwo\nuncommitted work\n");
    t.write("notes.txt", "untracked notes\n");
    let res = hist(&t, json!({ "op": "reset", "to": a.to_string(), "mode": "hard" })).unwrap();
    assert_eq!(head(&t), a);
    assert_eq!(t.read("a.txt"), "one\n", "hard: the files match the commit");
    // The uncommitted work that the reset overwrote is in the snapshot.
    let name = res["snapshot"]["name"].as_str().expect("a hard reset saves the changed files");
    safety::restore(&fresh(&t), name).unwrap();
    assert_eq!(t.read("a.txt"), "one\ntwo\nuncommitted work\n");
    // The removed commit is still there: resetting back to `before` undoes the reset.
    hist(&t, json!({ "op": "reset", "to": res["before"], "mode": "hard" })).unwrap();
    assert_eq!(head(&t), b);
    assert!(hist(&t, json!({ "op": "reset", "to": a.to_string(), "mode": "sideways" })).is_err());
}

/// A branch with four commits after the base, each adding one file.
fn four_commits() -> (TestRepo, Oid, Vec<Oid>) {
    let t = TestRepo::new();
    let base = t.commit_file("base.txt", "base\n", "base");
    let oids = ["one", "two", "three", "four"]
        .iter()
        .map(|n| t.commit_file(&format!("{n}.txt"), &format!("{n}\n"), &format!("commit {n}\n\nBody of {n}.")))
        .collect();
    (t, base, oids)
}

fn steps(pairs: &[(Oid, &str, Option<&str>)]) -> Value {
    json!(pairs.iter().map(|(o, a, m)| json!({ "oid": o.to_string(), "action": a, "message": m })).collect::<Vec<_>>())
}

fn full_message(t: &TestRepo, back: usize) -> String {
    let r = fresh(t);
    let mut c = r.head().unwrap().peel_to_commit().unwrap();
    for _ in 0..back {
        c = c.parent(0).unwrap();
    }
    String::from_utf8_lossy(c.message_bytes()).trim_end().to_string()
}

#[test]
fn rebase_plan_lists_the_commits_oldest_first() {
    let (t, base, oids) = four_commits();
    let plan = rewrite::rebase_plan(&fresh(&t), Some(&base.to_string())).unwrap();
    assert_eq!(plan.iter().map(|c| c.oid.clone()).collect::<Vec<_>>(), oids.iter().map(|o| o.to_string()).collect::<Vec<_>>());
    assert_eq!(plan[0].summary, "commit one");
    assert_eq!(plan[0].message, "commit one\n\nBody of one.");
    assert!(plan.iter().all(|c| !c.published && !c.merge));
    // Without a base the plan starts at the root commit.
    assert_eq!(rewrite::rebase_plan(&fresh(&t), None).unwrap().len(), 5);
    // The first two commits are on the upstream: they are marked.
    t.repo.remote("origin", "https://example.com/x.git").unwrap();
    t.set_ref("refs/remotes/origin/main", oids[1]);
    t.repo.find_branch("main", git2::BranchType::Local).unwrap().set_upstream(Some("origin/main")).unwrap();
    let plan = rewrite::rebase_plan(&fresh(&t), Some(&base.to_string())).unwrap();
    assert_eq!(plan.iter().map(|c| c.published).collect::<Vec<_>>(), vec![true, true, false, false]);
}

#[test]
fn interactive_rebase_reorders_and_drops() {
    if !has_git() {
        return;
    }
    let (t, base, o) = four_commits();
    let res = hist(
        &t,
        json!({ "op": "rebase-interactive", "base": base.to_string(), "steps": steps(&[(o[2], "pick", None), (o[0], "pick", None), (o[1], "drop", None), (o[3], "pick", None)]) }),
    )
    .unwrap();
    assert!(res.get("stopped").is_none(), "{res}");
    assert_eq!(res["before"], o[3].to_string());
    assert_eq!(log(&t), vec!["commit four", "commit one", "commit three", "base"]);
    assert!(!t.path().join("two.txt").exists(), "the dropped commit's file is gone");
    assert!(t.path().join("one.txt").exists() && t.path().join("three.txt").exists() && t.path().join("four.txt").exists());
    assert_eq!(state(&t), None);
    // The old commits are still reachable by id: a reset to `before` undoes the rebase.
    hist(&t, json!({ "op": "reset", "to": res["before"], "mode": "hard" })).unwrap();
    assert_eq!(log(&t), vec!["commit four", "commit three", "commit two", "commit one", "base"]);
}

#[test]
fn interactive_rebase_rewords_squashes_and_fixes_up() {
    if !has_git() {
        return;
    }
    let (t, base, o) = four_commits();
    hist(
        &t,
        json!({ "op": "rebase-interactive", "base": base.to_string(), "steps": steps(&[
            (o[0], "reword", Some("First, with a better message\n\nAnd a new body.")),
            (o[1], "pick", None),
            (o[2], "squash", None),
            (o[3], "fixup", None),
        ]) }),
    )
    .unwrap();
    assert_eq!(log(&t), vec!["commit two", "First, with a better message", "base"]);
    assert_eq!(full_message(&t, 1), "First, with a better message\n\nAnd a new body.");
    // squash keeps both messages, fixup drops its own.
    let melded = full_message(&t, 0);
    assert!(melded.contains("commit two") && melded.contains("Body of two."), "{melded}");
    assert!(melded.contains("commit three") && melded.contains("Body of three."), "{melded}");
    assert!(!melded.contains("commit four"), "{melded}");
    // All four files are there: nothing was lost by melding.
    for n in ["one", "two", "three", "four"] {
        assert!(t.path().join(format!("{n}.txt")).exists(), "{n}.txt");
    }
    assert!(!worktree::is_dirty(&fresh(&t)));
}

#[test]
fn interactive_rebase_squash_with_one_message_for_the_group() {
    if !has_git() {
        return;
    }
    let (t, base, o) = four_commits();
    hist(
        &t,
        json!({ "op": "rebase-interactive", "base": base.to_string(), "steps": steps(&[
            (o[0], "pick", Some("One commit for everything\n\nIt 'quotes' and $expands nothing.")),
            (o[1], "squash", None),
            (o[2], "squash", None),
            (o[3], "squash", None),
        ]) }),
    )
    .unwrap();
    assert_eq!(log(&t), vec!["One commit for everything", "base"]);
    assert_eq!(full_message(&t, 0), "One commit for everything\n\nIt 'quotes' and $expands nothing.");
    for n in ["one", "two", "three", "four"] {
        assert!(t.path().join(format!("{n}.txt")).exists());
    }
}

#[test]
fn interactive_rebase_refuses_invalid_plans() {
    if !has_git() {
        return;
    }
    let (t, base, o) = four_commits();
    let before = head(&t);
    let run = |s: Value| hist(&t, json!({ "op": "rebase-interactive", "base": base.to_string(), "steps": s }));

    let err = run(steps(&[(o[0], "squash", None), (o[1], "pick", None), (o[2], "pick", None), (o[3], "pick", None)])).unwrap_err();
    assert!(err.to_string().contains("first commit cannot be squashed"), "{err}");
    // A commit is missing from the steps: the list is out of date.
    let err = run(steps(&[(o[0], "pick", None), (o[1], "pick", None), (o[2], "pick", None)])).unwrap_err();
    assert!(err.to_string().contains("changed since"), "{err}");
    let err = run(steps(&[(o[0], "drop", None), (o[1], "drop", None), (o[2], "drop", None), (o[3], "drop", None)])).unwrap_err();
    assert!(err.to_string().contains("Every commit is dropped"), "{err}");
    let err = run(steps(&[(o[0], "reword", None), (o[1], "pick", None), (o[2], "pick", None), (o[3], "pick", None)])).unwrap_err();
    assert!(err.to_string().contains("Enter the new message"), "{err}");
    let err = run(steps(&[(o[0], "explode", None), (o[1], "pick", None), (o[2], "pick", None), (o[3], "pick", None)])).unwrap_err();
    assert!(err.to_string().contains("not a rebase action"), "{err}");
    assert_eq!(head(&t), before, "a refused plan changes nothing");
    assert_eq!(state(&t), None);

    // A range with a merge commit is refused.
    let (t, _, _) = diverged();
    op(&t, json!({ "op": "merge", "branch": "topic" })).unwrap();
    conflicts::resolve(&fresh(&t), "f.txt", Choice::Ours).unwrap();
    hist(&t, json!({ "op": "continue" })).unwrap();
    let plan = rewrite::rebase_plan(&fresh(&t), None).unwrap();
    assert!(plan.iter().any(|c| c.merge));
    let all: Vec<Value> = plan.iter().map(|c| json!({ "oid": c.oid, "action": "pick" })).collect();
    let err = hist(&t, json!({ "op": "rebase-interactive", "steps": all })).unwrap_err();
    assert!(err.to_string().contains("merge commits"), "{err}");
}

#[test]
fn interactive_rebase_stops_for_a_conflict_and_continues() {
    if !has_git() {
        return;
    }
    let t = TestRepo::new();
    let base = t.commit_file("f.txt", "one\n", "base");
    let a = t.commit_file("f.txt", "one\ntwo\n", "add two");
    let b = t.commit_file("f.txt", "one\ntwo\nthree\n", "add three");
    // Swapping the commits makes the second one apply to a file without "two".
    let res =
        hist(&t, json!({ "op": "rebase-interactive", "base": base.to_string(), "steps": steps(&[(b, "pick", None), (a, "pick", None)]) }))
            .unwrap();
    assert_eq!(res["stopped"], "rebase", "{res}");
    assert_eq!(conflict_paths(&t), vec!["f.txt"]);
    conflicts::resolve(&fresh(&t), "f.txt", Choice::Content { content: "one\nthree\n".into() }).unwrap();
    let res = hist(&t, json!({ "op": "continue" })).unwrap();
    // The next commit conflicts too.
    assert_eq!(res["stopped"], "rebase", "{res}");
    conflicts::resolve(&fresh(&t), "f.txt", Choice::Content { content: "one\ntwo\nthree\n".into() }).unwrap();
    hist(&t, json!({ "op": "continue" })).unwrap();
    assert_eq!(state(&t), None);
    assert_eq!(log(&t), vec!["add two", "add three", "base"]);
    assert_eq!(t.read("f.txt"), "one\ntwo\nthree\n");
}

#[test]
fn crlf_file_keeps_its_line_endings_when_resolved_with_text() {
    if !has_git() {
        return;
    }
    let t = TestRepo::new();
    t.commit_file("w.txt", "one\r\ntwo\r\nthree\r\n", "base");
    op(&t, json!({ "op": "branch-create", "name": "topic", "checkout": true })).unwrap();
    t.commit_file("w.txt", "one\r\nTWO topic\r\nthree\r\n", "topic");
    op(&t, json!({ "op": "checkout", "target": "main", "kind": "branch" })).unwrap();
    t.commit_file("w.txt", "one\r\nTWO main\r\nthree\r\n", "main");
    assert_eq!(op(&t, json!({ "op": "merge", "branch": "topic" })).unwrap()["stopped"], "merge");
    // A text area sends LF only.
    conflicts::resolve(&fresh(&t), "w.txt", Choice::Content { content: "one\nTWO both\nthree\n".into() }).unwrap();
    assert_eq!(t.read("w.txt"), "one\r\nTWO both\r\nthree\r\n");
}
