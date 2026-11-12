//! File history, blame, submodules and worktrees.

mod common;

use common::TestRepo;
use graftline::{extras, gitcli, repo};

#[test]
fn file_history_lists_only_commits_that_changed_the_file() {
    let t = TestRepo::new();
    let added = t.commit_file("a.txt", "one\n", "add a");
    t.commit_file("b.txt", "other\n", "unrelated");
    let changed = t.commit_file("a.txt", "one\ntwo\n", "change a");
    t.remove("a.txt");
    t.stage_all();
    let deleted = t.commit("delete a");
    t.commit_file("c.txt", "c\n", "after");

    let r = repo::open(&t.root()).unwrap();
    let (commits, more) = extras::file_history(&r, "a.txt", 100).unwrap();
    assert!(!more);
    let got: Vec<(String, char, &str)> = commits.iter().map(|c| (c.oid.clone(), c.status, c.summary.as_str())).collect();
    assert_eq!(
        got,
        vec![(deleted.to_string(), 'D', "delete a"), (changed.to_string(), 'M', "change a"), (added.to_string(), 'A', "add a")]
    );
    assert_eq!(commits[0].author, "Test User");

    let (limited, more) = extras::file_history(&r, "a.txt", 2).unwrap();
    assert_eq!(limited.len(), 2);
    assert!(more);
    assert!(extras::file_history(&r, "never-existed.txt", 10).unwrap().0.is_empty());
}

#[test]
fn file_history_skips_a_merge_that_only_brings_a_change_in() {
    let t = TestRepo::new();
    let base = t.commit_file("a.txt", "one\n", "base");
    // The side branch changes a.txt; main changes another file; the merge takes both.
    t.write("a.txt", "one\nside\n");
    t.stage_all();
    let side = t.commit("side changes a");
    let main = t.raw_commit("main work", &[base]);
    let merged_tree = {
        let side_tree = t.repo.find_commit(side).unwrap().tree().unwrap();
        let main_tree = t.repo.find_commit(main).unwrap().tree().unwrap();
        let mut b = t.repo.treebuilder(Some(&main_tree)).unwrap();
        b.insert("a.txt", side_tree.get_name("a.txt").unwrap().id(), 0o100644).unwrap();
        t.repo.find_tree(b.write().unwrap()).unwrap()
    };
    let sig = t.sig("Test User");
    let parents = [&t.repo.find_commit(main).unwrap(), &t.repo.find_commit(side).unwrap()];
    let merge = t.repo.commit(None, &sig, &sig, "merge side", &merged_tree, &parents).unwrap();
    t.set_ref("refs/heads/main", merge);

    let r = repo::open(&t.root()).unwrap();
    let (commits, _) = extras::file_history(&r, "a.txt", 100).unwrap();
    let summaries: Vec<&str> = commits.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(summaries, vec!["side changes a", "base"]);
}

#[test]
fn blame_names_the_commit_of_each_line() {
    let t = TestRepo::new();
    t.write("f.txt", "one\ntwo\nthree\n");
    t.stage_all();
    let first = t.commit_as("Ada Lovelace", "write three lines");
    t.write("f.txt", "one\nTWO\nthree\nfour\n");
    t.stage_all();
    let second = t.commit_as("Grace Hopper", "change two, add four");

    let r = repo::open(&t.root()).unwrap();
    let b = extras::blame(&r, "f.txt", None).unwrap();
    assert_eq!(b.lines, vec!["one", "TWO", "three", "four"]);
    assert_eq!(b.commit, second.to_string());
    // Every line belongs to exactly one hunk.
    let mut owner = vec![String::new(); b.lines.len()];
    for h in &b.hunks {
        for line in h.start..h.start + h.lines {
            assert!(owner[line - 1].is_empty(), "line {line} is in two hunks");
            owner[line - 1] = h.oid.clone();
        }
    }
    assert_eq!(owner, vec![first.to_string(), second.to_string(), first.to_string(), second.to_string()]);
    let hopper = b.hunks.iter().find(|h| h.oid == second.to_string()).unwrap();
    assert_eq!((hopper.author.as_str(), hopper.summary.as_str()), ("Grace Hopper", "change two, add four"));

    // The blame of the older version knows nothing about the later commit.
    let old = extras::blame(&r, "f.txt", Some(&first.to_string())).unwrap();
    assert_eq!(old.lines, vec!["one", "two", "three"]);
    assert!(old.hunks.iter().all(|h| h.oid == first.to_string()));

    assert!(extras::blame(&r, "missing.txt", None).is_err());
    t.write_bytes("b.bin", &[0u8, 1, 2, 0]);
    t.stage_all();
    t.commit("binary");
    assert!(extras::blame(&r, "b.bin", None).unwrap_err().to_string().contains("binary"));
}

#[test]
fn worktrees_and_submodules_are_listed() {
    let t = TestRepo::new();
    let a = t.commit_file("a.txt", "one\n", "first");
    let r = repo::open(&t.root()).unwrap();
    let none = extras::extras(&r).unwrap();
    assert!(none.submodules.is_empty() && none.worktrees.is_empty());

    // A linked worktree on its own branch.
    let wt_parent = tempfile::tempdir().unwrap();
    let wt_path = wt_parent.path().join("review");
    t.branch("review", a);
    let branch = t.repo.find_branch("review", git2::BranchType::Local).unwrap();
    let mut opts = git2::WorktreeAddOptions::new();
    opts.reference(Some(branch.get()));
    t.repo.worktree("review", &wt_path, Some(&opts)).unwrap();
    let listed = extras::extras(&repo::open(&t.root()).unwrap()).unwrap();
    assert_eq!(listed.worktrees.len(), 1);
    let wt = &listed.worktrees[0];
    assert_eq!((wt.name.as_str(), wt.branch.as_deref(), wt.locked, wt.missing), ("review", Some("review"), false, false));
    assert_eq!(std::path::PathBuf::from(&wt.path), repo::clean_path(&wt_path));

    // A submodule, added with git (libgit2 cannot clone it in one call).
    if gitcli::detect().is_none() {
        eprintln!("skipped the submodule part: git is not on PATH");
        return;
    }
    let lib = TestRepo::new();
    let lib_head = lib.commit_file("lib.txt", "library\n", "library commit");
    let url = lib.path().display().to_string().replace('\\', "/");
    gitcli::run(&t.root(), &["-c", "protocol.file.allow=always", "submodule", "add", &url, "vendor/lib"], None).unwrap();
    let listed = extras::extras(&repo::open(&t.root()).unwrap()).unwrap();
    assert_eq!(listed.submodules.len(), 1);
    let sm = &listed.submodules[0];
    assert_eq!(sm.path, "vendor/lib");
    assert!(sm.initialized);
    assert_eq!(sm.checked_out.as_deref(), Some(lib_head.to_string().as_str()));
    assert_eq!(sm.url.as_deref(), Some(url.as_str()));
    // The submodule folder opens as a repository of its own.
    assert_eq!(repo::discover(std::path::Path::new(&sm.full_path)).unwrap(), repo::clean_path(&t.path().join("vendor").join("lib")));
}
