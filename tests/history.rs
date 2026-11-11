//! History loading, ordering and graph layout on real (temporary) repositories.

mod common;

use common::TestRepo;
use git2::Oid;
use graftline::graph::EdgeKind;
use graftline::history::{self, CommitCache, History, Kind};

fn build(t: &TestRepo) -> History {
    let repo = graftline::repo::open(&t.root()).unwrap();
    let tips = history::read_tips(&repo).unwrap();
    history::build(&repo, &mut CommitCache::default(), tips, false).unwrap()
}

fn row(h: &History, oid: Oid) -> usize {
    *h.index.get(&oid).unwrap_or_else(|| panic!("{oid} is not in the history")) as usize
}

fn label_names(h: &History, oid: Oid) -> Vec<String> {
    h.labels.get(&oid).map(|l| l.iter().map(|l| format!("{}:{}", l.kind, l.name)).collect()).unwrap_or_default()
}

/// Every parent is below its child, and the lines of adjacent rows connect.
fn assert_well_formed(h: &History) {
    for (r, parents) in h.parents.iter().enumerate() {
        for &p in parents {
            assert!(p as usize > r, "parent row {p} is not below child row {r}");
        }
    }
    for r in 0..h.layout.len() {
        let mut bottom: Vec<u16> = h.layout.edges(r).iter().filter(|e| e.kind != EdgeKind::In).map(|e| e.col).collect();
        let mut top: Vec<u16> = if r + 1 < h.layout.len() {
            h.layout.edges(r + 1).iter().filter(|e| e.kind != EdgeKind::Out).map(|e| e.col).collect()
        } else {
            Vec::new()
        };
        bottom.sort_unstable();
        bottom.dedup();
        top.sort_unstable();
        top.dedup();
        assert_eq!(bottom, top, "lines break below row {r}");
    }
}

#[test]
fn linear_history_is_ordered_newest_first() {
    let t = TestRepo::new();
    let a = t.commit_file("a.txt", "1", "first");
    let b = t.commit_file("a.txt", "2", "second");
    let c = t.commit_file("a.txt", "3", "third");
    let h = build(&t);
    assert_eq!(h.commits.iter().map(|c| c.oid).collect::<Vec<_>>(), vec![c, b, a]);
    assert_eq!(h.layout.width, 1);
    assert_eq!(label_names(&h, c), vec!["branch:main"]);
    assert!(h.labels[&c][0].current);
    assert_eq!(&*h.commits[0].summary, "third");
    assert_eq!(h.author(h.commits[0].author), ("Test User", "test.user@example.com"));
    assert_well_formed(&h);
}

#[test]
fn empty_repository_has_no_rows() {
    let t = TestRepo::new();
    let h = build(&t);
    assert!(h.commits.is_empty());
    assert_eq!(h.layout.width, 0);
}

#[test]
fn merge_commit_gets_two_lanes() {
    let t = TestRepo::new();
    let base = t.commit_file("a.txt", "1", "base");
    let main2 = t.raw_commit("main work", &[base]);
    let feat = t.raw_commit("feature work", &[base]);
    let merge = t.raw_commit("merge feature", &[main2, feat]);
    t.set_ref("refs/heads/main", merge);
    t.branch("feature", feat);
    let h = build(&t);
    assert_eq!(h.commits.len(), 4);
    assert_eq!(row(&h, merge), 0);
    assert_eq!(row(&h, base), 3);
    assert_eq!(h.parents[0].len(), 2);
    assert_eq!(h.layout.width, 2);
    // The first parent stays in the merge commit's column, the merged branch is beside it.
    assert_eq!(h.layout.col(row(&h, main2)), h.layout.col(0));
    assert_ne!(h.layout.col(row(&h, feat)), h.layout.col(0));
    assert_ne!(h.layout.color(row(&h, feat)), h.layout.color(0));
    assert_well_formed(&h);
}

#[test]
fn octopus_merge_has_a_lane_per_parent() {
    let t = TestRepo::new();
    let base = t.commit_file("a.txt", "1", "base");
    let p1 = t.raw_commit("one", &[base]);
    let p2 = t.raw_commit("two", &[base]);
    let p3 = t.raw_commit("three", &[base]);
    let p4 = t.raw_commit("four", &[base]);
    let merge = t.raw_commit("octopus", &[p1, p2, p3, p4]);
    t.set_ref("refs/heads/main", merge);
    let h = build(&t);
    assert_eq!(h.parents[row(&h, merge)].len(), 4);
    assert_eq!(h.layout.width, 4);
    let mut cols: Vec<u16> = [p1, p2, p3, p4].iter().map(|p| h.layout.col(row(&h, *p))).collect();
    cols.sort_unstable();
    assert_eq!(cols, vec![0, 1, 2, 3]);
    let outs = h.layout.edges(0).iter().filter(|e| e.kind == EdgeKind::Out).count();
    assert_eq!(outs, 4);
    // All four lanes end in the base commit.
    let ins = h.layout.edges(row(&h, base)).iter().filter(|e| e.kind == EdgeKind::In).count();
    assert_eq!(ins, 4);
    assert_well_formed(&h);
}

#[test]
fn unrelated_histories_are_both_shown() {
    let t = TestRepo::new();
    let a1 = t.commit_file("a.txt", "1", "root a");
    let a2 = t.commit_file("a.txt", "2", "second a");
    let b1 = t.raw_commit("root b", &[]);
    let b2 = t.raw_commit("second b", &[b1]);
    t.branch("other", b2);
    let h = build(&t);
    assert_eq!(h.commits.len(), 4);
    let roots: Vec<Oid> = h.commits.iter().zip(&h.parents).filter(|(_, p)| p.is_empty()).map(|(c, _)| c.oid).collect();
    assert_eq!(roots.len(), 2);
    assert!(roots.contains(&a1) && roots.contains(&b1));
    assert!(row(&h, a2) < row(&h, a1) && row(&h, b2) < row(&h, b1));
    assert_well_formed(&h);
}

#[test]
fn detached_head_gets_its_own_label() {
    let t = TestRepo::new();
    let a = t.commit_file("a.txt", "1", "first");
    let b = t.commit_file("a.txt", "2", "second");
    t.repo.set_head_detached(a).unwrap();
    let h = build(&t);
    assert_eq!(label_names(&h, a), vec!["head:HEAD"]);
    assert!(h.labels[&a][0].current);
    assert_eq!(label_names(&h, b), vec!["branch:main"]);
    assert!(!h.labels[&b][0].current);
}

#[test]
fn detached_head_on_an_unreferenced_commit_is_still_shown() {
    let t = TestRepo::new();
    let a = t.commit_file("a.txt", "1", "first");
    let orphan = t.raw_commit("not on any branch", &[a]);
    t.repo.set_head_detached(orphan).unwrap();
    let h = build(&t);
    assert_eq!(h.commits.len(), 2);
    assert_eq!(row(&h, orphan), 0);
}

#[test]
fn tags_and_remote_branches_are_labelled() {
    let t = TestRepo::new();
    let a = t.commit_file("a.txt", "1", "first");
    let b = t.commit_file("a.txt", "2", "second");
    t.tag("v1", a, false);
    t.tag("v2", b, true);
    t.set_ref("refs/remotes/origin/main", a);
    t.repo.reference_symbolic("refs/remotes/origin/HEAD", "refs/remotes/origin/main", true, "test").unwrap();
    let h = build(&t);
    assert_eq!(label_names(&h, a), vec!["remote:origin/main", "tag:v1"]);
    // The annotated tag is peeled to its commit; the current branch comes first.
    assert_eq!(label_names(&h, b), vec!["branch:main", "tag:v2"]);
}

#[test]
fn stash_is_a_row_with_one_parent() {
    let mut t = TestRepo::new();
    let a = t.commit_file("a.txt", "1", "first");
    t.write("a.txt", "changed");
    t.write("new.txt", "untracked");
    let sig = t.sig("Test User");
    let stash = t.repo.stash_save(&sig, "work in progress", Some(git2::StashFlags::INCLUDE_UNTRACKED)).unwrap();
    let h = build(&t);
    // The stash commit has three parents (base, index, untracked files); only the base is drawn.
    assert_eq!(t.repo.find_commit(stash).unwrap().parent_count(), 3);
    assert_eq!(h.commits.len(), 2);
    let r = row(&h, stash);
    assert_eq!(h.commits[r].kind, Kind::Stash);
    assert_eq!(h.parents[r], vec![row(&h, a) as u32]);
    assert_eq!(label_names(&h, stash), vec!["stash:stash@{0}"]);
    assert_well_formed(&h);
}

#[test]
fn working_tree_row_sits_above_head() {
    let t = TestRepo::new();
    let a = t.commit_file("a.txt", "1", "first");
    let b = t.raw_commit("newer commit on another branch", &[a]);
    t.branch("other", b);
    let repo = graftline::repo::open(&t.root()).unwrap();
    let tips = history::read_tips(&repo).unwrap();
    let h = history::build(&repo, &mut CommitCache::default(), tips, true).unwrap();
    assert_eq!(h.commits.len(), 3);
    assert_eq!(h.commits[0].kind, Kind::Wip);
    assert!(h.commits[0].oid.is_zero());
    assert_eq!(h.parents[0], vec![row(&h, a) as u32]);
    assert_well_formed(&h);
}

#[test]
fn fingerprint_changes_only_when_refs_change() {
    let t = TestRepo::new();
    let a = t.commit_file("a.txt", "1", "first");
    let repo = graftline::repo::open(&t.root()).unwrap();
    let f1 = history::read_tips(&repo).unwrap().fingerprint;
    t.write("untracked.txt", "x");
    assert_eq!(history::read_tips(&repo).unwrap().fingerprint, f1);
    t.branch("topic", a);
    let f2 = history::read_tips(&repo).unwrap().fingerprint;
    assert_ne!(f1, f2);
    t.repo.set_head("refs/heads/topic").unwrap();
    assert_ne!(history::read_tips(&repo).unwrap().fingerprint, f2);
}

#[test]
fn search_finds_message_author_and_hash() {
    let t = TestRepo::new();
    t.write("a.txt", "1");
    t.stage_all();
    let a = t.commit_as("Ada Lovelace", "Add the engine");
    t.write("a.txt", "2");
    t.stage_all();
    let b = t.commit_as("Grace Hopper", "Fix the COMPILER bug");
    let h = build(&t);
    assert_eq!(h.search("compiler"), vec![row(&h, b) as u32]);
    assert_eq!(h.search("ada"), vec![row(&h, a) as u32]);
    assert_eq!(h.search("hopper@example"), vec![row(&h, b) as u32]);
    assert_eq!(h.search(&a.to_string()[..8]), vec![row(&h, a) as u32]);
    assert_eq!(h.search("the").len(), 2);
    assert!(h.search("nothing like this").is_empty());
    assert!(h.search("  ").is_empty());
}

#[test]
fn commit_cache_survives_a_restart() {
    let t = TestRepo::new();
    let mut tip = t.commit_file("a.txt", "0", "c0");
    for i in 1..40 {
        tip = t.raw_commit(&format!("c{i}"), &[tip]);
    }
    t.set_ref("refs/heads/main", tip);
    let repo = graftline::repo::open(&t.root()).unwrap();
    let mut cache = CommitCache::default();
    let h1 = history::build(&repo, &mut cache, history::read_tips(&repo).unwrap(), false).unwrap();
    assert_eq!(h1.read, 40);
    let file = t.path().join(".git").join("graftline-test.commits");
    cache.save(&file, Some(&h1.oids())).unwrap();

    let mut restored = CommitCache::load_file(&file);
    assert_eq!(restored.len(), 40);
    let h2 = history::build(&repo, &mut restored, history::read_tips(&repo).unwrap(), false).unwrap();
    assert_eq!(h2.read, 0, "all commits must come from the cache file");
    let summaries =
        |h: &History| h.commits.iter().map(|c| (c.oid, c.summary.to_string(), h.author(c.author).0.to_string())).collect::<Vec<_>>();
    assert_eq!(summaries(&h1), summaries(&h2));

    // One more commit: only that one is read.
    let newer = t.raw_commit("newer", &[tip]);
    t.set_ref("refs/heads/main", newer);
    let h3 = history::build(&repo, &mut restored, history::read_tips(&repo).unwrap(), false).unwrap();
    assert_eq!(h3.read, 1);
    assert_eq!(h3.commits.len(), 41);
}

#[test]
fn damaged_cache_file_is_ignored() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "1", "first");
    let file = t.path().join(".git").join("bad.commits");
    std::fs::write(&file, b"GLCACHE1\xff\xff\xff\xff garbage").unwrap();
    assert!(CommitCache::load_file(&file).is_empty());
    std::fs::write(&file, b"not a cache").unwrap();
    assert!(CommitCache::load_file(&file).is_empty());
    assert!(CommitCache::load_file(&t.path().join("missing")).is_empty());
}

#[test]
fn large_history_is_read_completely_on_several_threads() {
    let t = TestRepo::new();
    // 1500 commits: a main chain with a side branch merged every 50 commits.
    let mut tip = t.commit_file("a.txt", "0", "root");
    let mut count = 1;
    for i in 0..500 {
        if i % 50 == 10 {
            let side1 = t.raw_commit(&format!("side {i} a"), &[tip]);
            let side2 = t.raw_commit(&format!("side {i} b"), &[side1]);
            let main = t.raw_commit(&format!("main {i}"), &[tip]);
            tip = t.raw_commit(&format!("merge {i}"), &[main, side2]);
            count += 4;
        } else {
            tip = t.raw_commit(&format!("main {i}"), &[tip]);
            count += 1;
        }
    }
    let mut other = t.raw_commit("other root", &[]);
    for i in 0..960 {
        other = t.raw_commit(&format!("other {i}"), &[other]);
    }
    count += 961;
    t.set_ref("refs/heads/main", tip);
    t.branch("other", other);
    let h = build(&t);
    assert_eq!(h.commits.len(), count);
    assert_eq!(h.read, count);
    assert_eq!(h.index.len(), count);
    assert_well_formed(&h);
}
