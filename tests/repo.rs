//! Refs, commit details and diffs read from temporary repositories.

mod common;

use common::TestRepo;
use graftline::repo;

#[test]
fn discover_finds_the_root_from_a_subfolder() {
    let t = TestRepo::new();
    t.commit_file("src/deep/a.txt", "1", "first");
    let root = repo::discover(&t.path().join("src").join("deep")).unwrap();
    assert_eq!(root, repo::clean_path(t.path()));
    assert!(repo::discover(std::env::temp_dir().join("graftline-no-such-dir-xyz").as_path()).is_err());
}

#[test]
fn refs_list_branches_remotes_tags_and_stashes() {
    let mut t = TestRepo::new();
    let a = t.commit_file("a.txt", "1", "first");
    let b = t.commit_file("a.txt", "2", "second");
    let c = t.commit_file("a.txt", "3", "third");
    t.branch("feature/login", a);
    t.repo.remote("origin", "https://example.com/demo.git").unwrap();
    t.set_ref("refs/remotes/origin/main", b);
    t.set_ref("refs/remotes/origin/feature/login", a);
    t.repo.reference_symbolic("refs/remotes/origin/HEAD", "refs/remotes/origin/main", true, "test").unwrap();
    {
        let mut cfg = t.repo.config().unwrap();
        cfg.set_str("branch.main.remote", "origin").unwrap();
        cfg.set_str("branch.main.merge", "refs/heads/main").unwrap();
        cfg.set_str("branch.feature/login.remote", "origin").unwrap();
        cfg.set_str("branch.feature/login.merge", "refs/heads/deleted-upstream").unwrap();
    }
    t.tag("v1.9", a, false);
    t.tag("v1.10", b, true);
    t.write("a.txt", "dirty");
    let sig = t.sig("Test User");
    t.repo.stash_save(&sig, "half done", None).unwrap();

    let r = repo::open(&t.root()).unwrap();
    let refs = repo::refs(&r).unwrap();
    assert_eq!(refs.head.branch.as_deref(), Some("main"));
    assert_eq!(refs.head.oid.as_deref(), Some(c.to_string().as_str()));
    assert!(!refs.head.detached && !refs.head.unborn);
    assert_eq!(refs.state, None);

    let names: Vec<&str> = refs.branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, vec!["feature/login", "main"]);
    let main = &refs.branches[1];
    assert!(main.current);
    assert_eq!(main.upstream.as_deref(), Some("origin/main"));
    assert_eq!((main.ahead, main.behind), (1, 0));
    assert!(!main.gone);
    let login = &refs.branches[0];
    assert!(login.gone, "an upstream that does not exist is reported as gone");

    assert_eq!(refs.remotes.len(), 1);
    assert_eq!(refs.remotes[0].name, "origin");
    assert_eq!(refs.remotes[0].url.as_deref(), Some("https://example.com/demo.git"));
    let remote_names: Vec<&str> = refs.remotes[0].branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(remote_names, vec!["feature/login", "main"]);
    assert_eq!(refs.remotes[0].branches[1].full, "origin/main");

    // Newest version first, with numeric ordering: v1.10 is after v1.9.
    let tags: Vec<(&str, bool)> = refs.tags.iter().map(|t| (t.name.as_str(), t.annotated)).collect();
    assert_eq!(tags, vec![("v1.10", true), ("v1.9", false)]);
    assert_eq!(refs.tags[0].oid, b.to_string());

    assert_eq!(refs.stashes.len(), 1);
    assert!(refs.stashes[0].message.contains("half done"));
}

#[test]
fn unborn_and_detached_heads_are_reported() {
    let t = TestRepo::new();
    let r = repo::open(&t.root()).unwrap();
    let head = repo::head(&r);
    assert!(head.unborn && !head.detached);
    assert_eq!(head.branch.as_deref(), Some("main"));

    let a = t.commit_file("a.txt", "1", "first");
    t.repo.set_head_detached(a).unwrap();
    let head = repo::head(&r);
    assert!(head.detached && !head.unborn);
    assert_eq!(head.branch, None);
    assert_eq!(head.oid.as_deref(), Some(a.to_string().as_str()));
}

#[test]
fn commit_detail_lists_changes_with_line_counts() {
    let t = TestRepo::new();
    t.write("keep.txt", "one\ntwo\nthree\n");
    t.write("gone.txt", "bye\n");
    t.write("old-name.txt", "a long enough text\nto be detected as a rename\nwhen the file moves\n");
    t.stage_all();
    t.commit("base");
    t.write("keep.txt", "one\n2\nthree\nfour\n");
    t.remove("gone.txt");
    std::fs::rename(t.path().join("old-name.txt"), t.path().join("new-name.txt")).unwrap();
    t.write("dir/added.txt", "new\nfile\n");
    t.write_bytes("image.bin", &[0u8, 159, 146, 150, 0, 1, 2, 3]);
    t.stage_all();
    let oid = t.commit_as("Ada Lovelace", "Change several files\n\nA body with\ntwo lines.\n");

    let r = repo::open(&t.root()).unwrap();
    let d = repo::commit_detail(&r, oid).unwrap();
    assert_eq!(d.summary, "Change several files");
    assert_eq!(d.body, "A body with\ntwo lines.");
    assert_eq!(d.author.name, "Ada Lovelace");
    assert_eq!(d.parents.len(), 1);

    let by_path = |p: &str| d.files.iter().find(|f| f.path == p).unwrap_or_else(|| panic!("{p} missing in {:?}", d.files));
    let keep = by_path("keep.txt");
    assert_eq!((keep.status, keep.additions, keep.deletions), ('M', Some(2), Some(1)));
    assert_eq!(by_path("gone.txt").status, 'D');
    let added = by_path("dir/added.txt");
    assert_eq!((added.status, added.additions), ('A', Some(2)));
    let renamed = by_path("new-name.txt");
    assert_eq!(renamed.status, 'R');
    assert_eq!(renamed.old_path.as_deref(), Some("old-name.txt"));
    let bin = by_path("image.bin");
    assert!(bin.binary);
    assert_eq!(d.files.len(), 5);
    assert_eq!(d.additions, 4);
    assert_eq!(d.deletions, 2);
}

#[test]
fn root_commit_is_compared_with_the_empty_tree() {
    let t = TestRepo::new();
    let a = t.commit_file("a.txt", "x\ny\n", "first");
    let r = repo::open(&t.root()).unwrap();
    let d = repo::commit_detail(&r, a).unwrap();
    assert!(d.parents.is_empty());
    assert_eq!(d.files.len(), 1);
    assert_eq!((d.files[0].status, d.files[0].additions), ('A', Some(2)));
}

#[test]
fn file_diff_has_hunks_with_line_numbers() {
    let t = TestRepo::new();
    let lines: Vec<String> = (1..=30).map(|i| format!("line {i}")).collect();
    t.commit_file("a.txt", &(lines.join("\n") + "\n"), "base");
    let mut changed = lines.clone();
    changed[1] = "line two".into();
    changed.remove(27);
    changed.push("line 31".into());
    let oid = t.commit_file("a.txt", &(changed.join("\n") + "\n"), "edit");

    let r = repo::open(&t.root()).unwrap();
    let d = repo::commit_file_diff(&r, oid, "a.txt", None).unwrap();
    assert_eq!(d.status, 'M');
    assert!(!d.binary && !d.truncated);
    assert_eq!(d.hunks.len(), 2);
    let h0 = &d.hunks[0];
    assert_eq!((h0.old_start, h0.new_start), (1, 1));
    let removed: Vec<&str> = h0.lines.iter().filter(|l| l.t == '-').map(|l| l.s.as_str()).collect();
    let added: Vec<&str> = h0.lines.iter().filter(|l| l.t == '+').map(|l| l.s.as_str()).collect();
    assert_eq!(removed, vec!["line 2"]);
    assert_eq!(added, vec!["line two"]);
    let minus = h0.lines.iter().find(|l| l.t == '-').unwrap();
    assert_eq!((minus.o, minus.n), (Some(2), None));
    let plus = h0.lines.iter().find(|l| l.t == '+').unwrap();
    assert_eq!((plus.o, plus.n), (None, Some(2)));
    let ctx = h0.lines.iter().find(|l| l.t == ' ' && l.s == "line 3").unwrap();
    assert_eq!((ctx.o, ctx.n), (Some(3), Some(3)));
    let h1 = &d.hunks[1];
    assert!(h1.lines.iter().any(|l| l.t == '-' && l.s == "line 28"));
    assert!(h1.lines.iter().any(|l| l.t == '+' && l.s == "line 31"));
    assert!(h1.header.starts_with("@@ "));
}

#[test]
fn file_diff_marks_a_missing_final_newline() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "one\ntwo\n", "base");
    let oid = t.commit_file("a.txt", "one\ntwo", "drop the newline");
    let r = repo::open(&t.root()).unwrap();
    let d = repo::commit_file_diff(&r, oid, "a.txt", None).unwrap();
    let kinds: String = d.hunks[0].lines.iter().map(|l| l.t).collect();
    assert_eq!(kinds, " -+\\");
    assert_eq!(d.hunks[0].lines[3].s, "No newline at end of file");
}

#[test]
fn file_diff_of_a_rename_keeps_both_paths() {
    let t = TestRepo::new();
    t.commit_file("old.txt", "alpha\nbeta\ngamma\ndelta\nepsilon\n", "base");
    std::fs::rename(t.path().join("old.txt"), t.path().join("new.txt")).unwrap();
    t.write("new.txt", "alpha\nbeta\ngamma\ndelta\nEPSILON\n");
    t.stage_all();
    let oid = t.commit("rename and edit");
    let r = repo::open(&t.root()).unwrap();
    let d = repo::commit_file_diff(&r, oid, "new.txt", Some("old.txt")).unwrap();
    assert_eq!(d.status, 'R');
    assert_eq!(d.old_path.as_deref(), Some("old.txt"));
    assert_eq!(d.hunks.len(), 1);
    assert!(d.hunks[0].lines.iter().any(|l| l.t == '+' && l.s == "EPSILON"));
}

#[test]
fn binary_file_diff_has_no_hunks() {
    let t = TestRepo::new();
    t.write_bytes("b.bin", &[0u8, 1, 2, 3, 0, 255]);
    t.stage_all();
    t.commit("base");
    t.write_bytes("b.bin", &[0u8, 9, 9, 9, 0, 255, 7]);
    t.stage_all();
    let oid = t.commit("change binary");
    let r = repo::open(&t.root()).unwrap();
    let d = repo::commit_file_diff(&r, oid, "b.bin", None).unwrap();
    assert!(d.binary);
    assert!(d.hunks.is_empty());
    assert!(repo::commit_file_diff(&r, oid, "missing.txt", None).is_err());
}

#[test]
fn crlf_line_endings_do_not_leak_into_lines() {
    let t = TestRepo::new();
    t.commit_file("w.txt", "one\r\ntwo\r\n", "base");
    let oid = t.commit_file("w.txt", "one\r\nTWO\r\n", "edit");
    let r = repo::open(&t.root()).unwrap();
    let d = repo::commit_file_diff(&r, oid, "w.txt", None).unwrap();
    assert!(d.hunks[0].lines.iter().all(|l| !l.s.ends_with('\r')));
    assert!(d.hunks[0].lines.iter().any(|l| l.t == '+' && l.s == "TWO"));
}
