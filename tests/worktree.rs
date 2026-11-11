//! Status, staging by file, hunk and line, discard with snapshots, and commit.

mod common;

use common::TestRepo;
use graftline::repo::FileDiff;
use graftline::worktree::{self, CommitRequest, HunkSelection, PatchOp, Side};
use graftline::{repo, safety};

fn numbered(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

/// Replaces `line N` texts in a numbered file.
fn edit(text: &str, changes: &[(&str, &str)]) -> String {
    let mut out = text.to_string();
    for (from, to) in changes {
        out = out.replace(&format!("{from}\n"), to);
    }
    out
}

/// Content of a file as the index has it.
fn index_content(t: &TestRepo, path: &str) -> String {
    let mut index = t.repo.index().unwrap();
    index.read(true).unwrap();
    let entry = index.get_path(std::path::Path::new(path), 0).unwrap_or_else(|| panic!("{path} is not in the index"));
    String::from_utf8(t.repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap()
}

fn whole(diff: &FileDiff, hunk: usize) -> HunkSelection {
    HunkSelection { index: hunk, header: diff.hunks[hunk].header.clone(), lines: None }
}

/// Selects the changed lines of a hunk whose text is in `texts`.
fn lines(diff: &FileDiff, hunk: usize, texts: &[&str]) -> HunkSelection {
    let picked: Vec<usize> =
        diff.hunks[hunk].lines.iter().enumerate().filter(|(_, l)| l.t != ' ' && texts.contains(&l.s.as_str())).map(|(i, _)| i).collect();
    assert_eq!(picked.len(), texts.len(), "not all of {texts:?} are changed lines of hunk {hunk}");
    HunkSelection { index: hunk, header: diff.hunks[hunk].header.clone(), lines: Some(picked) }
}

fn status_of(t: &TestRepo) -> (Vec<String>, Vec<String>) {
    let s = worktree::status(&t.repo).unwrap();
    let fmt = |v: &[repo::FileChange]| v.iter().map(|f| format!("{} {}", f.status, f.path)).collect::<Vec<_>>();
    (fmt(&s.staged), fmt(&s.unstaged))
}

#[test]
fn status_separates_staged_and_unstaged_changes() {
    let t = TestRepo::new();
    t.write("modified.txt", "one\n");
    t.write("deleted.txt", "bye\n");
    t.write("staged-edit.txt", "v1\n");
    t.write("renamed-old.txt", "a text long enough\nfor rename detection\nto match the file\n");
    t.stage_all();
    t.commit("base");
    assert!(!worktree::is_dirty(&t.repo));

    t.write("modified.txt", "one\ntwo\n");
    t.remove("deleted.txt");
    t.write("dir/untracked.txt", "new\n");
    t.write("staged-edit.txt", "v2\n");
    t.write("staged-new.txt", "fresh\n");
    std::fs::rename(t.path().join("renamed-old.txt"), t.path().join("renamed-new.txt")).unwrap();
    worktree::stage(&t.repo, &["staged-edit.txt".into(), "staged-new.txt".into(), "renamed-old.txt".into(), "renamed-new.txt".into()])
        .unwrap();
    // A further edit of a staged file is unstaged.
    t.write("staged-edit.txt", "v3\n");

    assert!(worktree::is_dirty(&t.repo));
    let (staged, unstaged) = status_of(&t);
    assert_eq!(staged, vec!["R renamed-new.txt", "M staged-edit.txt", "A staged-new.txt"]);
    assert_eq!(unstaged, vec!["D deleted.txt", "? dir/untracked.txt", "M modified.txt", "M staged-edit.txt"]);
    let s = worktree::status(&t.repo).unwrap();
    assert_eq!(s.staged[0].old_path.as_deref(), Some("renamed-old.txt"));
    assert!(s.conflicted.is_empty());
}

#[test]
fn ignored_files_do_not_make_the_tree_dirty() {
    let t = TestRepo::new();
    t.commit_file(".gitignore", "target/\n*.log\n", "ignore build output");
    t.write("target/out.bin", "x");
    t.write("debug.log", "x");
    assert!(!worktree::is_dirty(&t.repo));
    assert!(worktree::status(&t.repo).unwrap().is_clean());
}

#[test]
fn stage_and_unstage_whole_files() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "one\n", "base");
    t.write("a.txt", "one\ntwo\n");
    t.write("new.txt", "new\n");
    worktree::stage(&t.repo, &["a.txt".into(), "new.txt".into()]).unwrap();
    assert_eq!(status_of(&t), (vec!["M a.txt".to_string(), "A new.txt".to_string()], vec![]));
    assert_eq!(index_content(&t, "a.txt"), "one\ntwo\n");

    worktree::unstage(&t.repo, &["a.txt".into(), "new.txt".into()]).unwrap();
    assert_eq!(status_of(&t), (vec![], vec!["M a.txt".to_string(), "? new.txt".to_string()]));
    // Unstaging never touches the working tree.
    assert_eq!(t.read("a.txt"), "one\ntwo\n");
    assert_eq!(t.read("new.txt"), "new\n");

    // A deletion is staged and unstaged the same way.
    t.remove("a.txt");
    worktree::stage(&t.repo, &["a.txt".into()]).unwrap();
    assert!(status_of(&t).0.contains(&"D a.txt".to_string()));
    worktree::unstage(&t.repo, &["a.txt".into()]).unwrap();
    assert!(status_of(&t).1.contains(&"D a.txt".to_string()));
}

#[test]
fn unstage_works_before_the_first_commit() {
    let t = TestRepo::new();
    t.write("a.txt", "one\n");
    worktree::stage(&t.repo, &["a.txt".into()]).unwrap();
    assert_eq!(status_of(&t).0, vec!["A a.txt"]);
    worktree::unstage(&t.repo, &["a.txt".into()]).unwrap();
    assert_eq!(status_of(&t), (vec![], vec!["? a.txt".to_string()]));
}

#[test]
fn stage_one_hunk_of_two() {
    let t = TestRepo::new();
    let base = numbered(30);
    t.commit_file("f.txt", &base, "base");
    t.write("f.txt", &edit(&base, &[("line 2", "line two\n"), ("line 28", "line twenty-eight\n")]));
    let diff = worktree::file_diff(&t.repo, Side::Unstaged, "f.txt", None).unwrap();
    assert_eq!(diff.hunks.len(), 2);

    worktree::apply_selection(&t.repo, PatchOp::Stage, "f.txt", &[whole(&diff, 1)]).unwrap();
    assert_eq!(index_content(&t, "f.txt"), edit(&base, &[("line 28", "line twenty-eight\n")]));
    // The other hunk is still unstaged; the working tree is unchanged.
    let rest = worktree::file_diff(&t.repo, Side::Unstaged, "f.txt", None).unwrap();
    assert_eq!(rest.hunks.len(), 1);
    assert!(rest.hunks[0].lines.iter().any(|l| l.t == '+' && l.s == "line two"));
    assert!(t.read("f.txt").contains("line two\n") && t.read("f.txt").contains("line twenty-eight\n"));
}

#[test]
fn stage_both_hunks_in_one_request() {
    let t = TestRepo::new();
    let base = numbered(30);
    t.commit_file("f.txt", &base, "base");
    // The first hunk adds lines, so the second one moves down in the new file.
    let changed = edit(&base, &[("line 2", "line 2\nextra a\nextra b\n"), ("line 28", "")]);
    t.write("f.txt", &changed);
    let diff = worktree::file_diff(&t.repo, Side::Unstaged, "f.txt", None).unwrap();
    worktree::apply_selection(&t.repo, PatchOp::Stage, "f.txt", &[whole(&diff, 1), whole(&diff, 0)]).unwrap();
    assert_eq!(index_content(&t, "f.txt"), changed);
    assert!(status_of(&t).1.is_empty());
}

#[test]
fn stage_selected_lines_only() {
    let t = TestRepo::new();
    let base = numbered(10);
    t.commit_file("f.txt", &base, "base");
    // One block: remove 4 and 5, add three lines.
    t.write("f.txt", &edit(&base, &[("line 4", "new a\nnew b\nnew c\n"), ("line 5", "")]));
    let diff = worktree::file_diff(&t.repo, Side::Unstaged, "f.txt", None).unwrap();
    assert_eq!(diff.hunks.len(), 1);

    // Stage the removal of line 4 and the addition of "new b" only.
    worktree::apply_selection(&t.repo, PatchOp::Stage, "f.txt", &[lines(&diff, 0, &["line 4", "new b"])]).unwrap();
    // Line 5 was not selected, so it stays where it is and the added line follows it.
    // `git add -p` gives the same result.
    assert_eq!(index_content(&t, "f.txt"), edit(&base, &[("line 4", ""), ("line 5", "line 5\nnew b\n")]));

    // What is left unstaged: remove line 5, add "new a" and "new c".
    let rest = worktree::file_diff(&t.repo, Side::Unstaged, "f.txt", None).unwrap();
    let mut changed: Vec<String> = rest.hunks[0].lines.iter().filter(|l| l.t != ' ').map(|l| format!("{}{}", l.t, l.s)).collect();
    changed.sort();
    assert_eq!(changed, vec!["+new a", "+new c", "-line 5"]);

    // Staging the rest gives the working-tree file.
    worktree::apply_selection(&t.repo, PatchOp::Stage, "f.txt", &[whole(&rest, 0)]).unwrap();
    assert_eq!(index_content(&t, "f.txt"), t.read("f.txt"));
}

#[test]
fn unstage_selected_lines_only() {
    let t = TestRepo::new();
    let base = numbered(10);
    t.commit_file("f.txt", &base, "base");
    let changed = edit(&base, &[("line 4", "new a\nnew b\n"), ("line 8", "")]);
    t.write("f.txt", &changed);
    worktree::stage(&t.repo, &["f.txt".into()]).unwrap();
    let diff = worktree::file_diff(&t.repo, Side::Staged, "f.txt", None).unwrap();

    // Take "new a" and the removal of line 8 out of the index.
    let hunk_of = |text: &str| diff.hunks.iter().position(|h| h.lines.iter().any(|l| l.t != ' ' && l.s == text)).unwrap();
    let (h_a, h_8) = (hunk_of("new a"), hunk_of("line 8"));
    let sel = if h_a == h_8 {
        vec![lines(&diff, h_a, &["new a", "line 8"])]
    } else {
        vec![lines(&diff, h_a, &["new a"]), lines(&diff, h_8, &["line 8"])]
    };
    worktree::apply_selection(&t.repo, PatchOp::Unstage, "f.txt", &sel).unwrap();
    assert_eq!(index_content(&t, "f.txt"), edit(&base, &[("line 4", "new b\n")]));
    assert_eq!(t.read("f.txt"), changed, "unstaging must not touch the working tree");
}

#[test]
fn discard_selected_lines_keeps_the_rest_and_can_be_undone() {
    let t = TestRepo::new();
    let base = numbered(10);
    t.commit_file("f.txt", &base, "base");
    let changed = edit(&base, &[("line 3", "line three\n"), ("line 7", "line 7\nadded after seven\n")]);
    t.write("f.txt", &changed);
    let diff = worktree::file_diff(&t.repo, Side::Unstaged, "f.txt", None).unwrap();
    let hunk = diff.hunks.iter().position(|h| h.lines.iter().any(|l| l.s == "added after seven")).unwrap();

    let snapshot = worktree::apply_selection(&t.repo, PatchOp::Discard, "f.txt", &[lines(&diff, hunk, &["added after seven"])]).unwrap();
    assert_eq!(t.read("f.txt"), edit(&base, &[("line 3", "line three\n")]));
    assert_eq!(index_content(&t, "f.txt"), base, "discard must not touch the index");

    // The snapshot has the file as it was before the discard.
    let snapshot = snapshot.expect("a discard saves a snapshot");
    assert_eq!(snapshot.files, vec!["f.txt"]);
    let restored = safety::restore(&t.repo, &snapshot.name).unwrap();
    assert_eq!(restored, vec!["f.txt"]);
    assert_eq!(t.read("f.txt"), changed);
}

#[test]
fn discard_files_restores_tracked_and_deletes_untracked() {
    let t = TestRepo::new();
    t.write("tracked.txt", "original\n");
    t.write("gone.txt", "will be deleted\n");
    t.stage_all();
    t.commit("base");
    t.write("tracked.txt", "edited\n");
    t.remove("gone.txt");
    t.write("deep/dir/untracked.txt", "precious notes\n");
    t.write("keep.txt", "another untracked file\n");

    let snapshot =
        worktree::discard(&t.repo, &["tracked.txt".into(), "gone.txt".into(), "deep/dir/untracked.txt".into()]).unwrap().unwrap();
    assert_eq!(t.read("tracked.txt"), "original\n");
    assert_eq!(t.read("gone.txt"), "will be deleted\n");
    assert!(!t.path().join("deep").exists(), "empty folders of a deleted untracked file are removed");
    assert_eq!(t.read("keep.txt"), "another untracked file\n", "files that were not named stay");
    assert_eq!(status_of(&t).1, vec!["? keep.txt"]);

    // Both files that existed are in the snapshot, and restoring brings them back.
    let mut files = snapshot.files.clone();
    files.sort();
    assert_eq!(files, vec!["deep/dir/untracked.txt", "tracked.txt"]);
    assert_eq!(safety::list(&t.repo).unwrap()[0].name, snapshot.name);
    safety::restore(&t.repo, &snapshot.name).unwrap();
    assert_eq!(t.read("tracked.txt"), "edited\n");
    assert_eq!(t.read("deep/dir/untracked.txt"), "precious notes\n");
}

#[test]
fn discard_keeps_staged_changes_of_the_same_file() {
    let t = TestRepo::new();
    t.commit_file("f.txt", "v1\n", "base");
    t.write("f.txt", "v2\n");
    worktree::stage(&t.repo, &["f.txt".into()]).unwrap();
    t.write("f.txt", "v3\n");
    worktree::discard(&t.repo, &["f.txt".into()]).unwrap();
    assert_eq!(t.read("f.txt"), "v2\n", "the file returns to its staged version, not to HEAD");
    assert_eq!(index_content(&t, "f.txt"), "v2\n");
}

#[test]
fn snapshots_can_be_listed_and_dropped_and_do_not_show_in_the_graph() {
    let t = TestRepo::new();
    t.commit_file("f.txt", "v1\n", "base");
    t.write("f.txt", "v2\n");
    let s1 = worktree::discard(&t.repo, &["f.txt".into()]).unwrap().unwrap();
    t.write("f.txt", "v3\n");
    let s2 = worktree::discard(&t.repo, &["f.txt".into()]).unwrap().unwrap();
    let names: Vec<String> = safety::list(&t.repo).unwrap().into_iter().map(|s| s.name).collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&s1.name) && names.contains(&s2.name));

    let tips = graftline::history::read_tips(&t.repo).unwrap();
    assert_eq!(tips.tips.len(), 2, "only the branch and HEAD are graph tips: {:?}", tips.tips);

    safety::drop(&t.repo, &s1.name).unwrap();
    assert_eq!(safety::list(&t.repo).unwrap().len(), 1);
    assert!(safety::restore(&t.repo, &s1.name).is_err());
    assert!(safety::restore(&t.repo, "../../heads/main").is_err());
}

#[test]
fn stage_part_of_an_untracked_file() {
    let t = TestRepo::new();
    t.commit_file("other.txt", "x\n", "base");
    t.write("new.txt", "alpha\nbeta\ngamma\n");
    let diff = worktree::file_diff(&t.repo, Side::Unstaged, "new.txt", None).unwrap();
    assert_eq!(diff.status, '?');
    worktree::apply_selection(&t.repo, PatchOp::Stage, "new.txt", &[lines(&diff, 0, &["alpha", "gamma"])]).unwrap();
    assert_eq!(index_content(&t, "new.txt"), "alpha\ngamma\n");
    assert_eq!(status_of(&t), (vec!["A new.txt".to_string()], vec!["M new.txt".to_string()]));
    assert_eq!(t.read("new.txt"), "alpha\nbeta\ngamma\n");
}

#[test]
fn paths_with_spaces_and_unicode_work() {
    let t = TestRepo::new();
    let path = "мої файли/звіт 2026.txt";
    let base = numbered(8);
    t.commit_file(path, &base, "base");
    t.write(path, &edit(&base, &[("line 2", "рядок два\n"), ("line 7", "line 7\ntail\n")]));
    let diff = worktree::file_diff(&t.repo, Side::Unstaged, path, None).unwrap();
    let hunk = diff.hunks.iter().position(|h| h.lines.iter().any(|l| l.s == "рядок два")).unwrap();
    worktree::apply_selection(&t.repo, PatchOp::Stage, path, &[lines(&diff, hunk, &["line 2", "рядок два"])]).unwrap();
    assert_eq!(index_content(&t, path), edit(&base, &[("line 2", "рядок два\n")]));
    let staged = worktree::file_diff(&t.repo, Side::Staged, path, None).unwrap();
    worktree::apply_selection(&t.repo, PatchOp::Unstage, path, &[whole(&staged, 0)]).unwrap();
    assert_eq!(index_content(&t, path), base);
}

#[test]
fn crlf_files_keep_their_line_endings() {
    let t = TestRepo::new();
    let base = "one\r\ntwo\r\nthree\r\nfour\r\n";
    t.commit_file("w.txt", base, "base");
    t.write("w.txt", "one\r\nTWO\r\nthree\r\nfour\r\nfive\r\n");
    let diff = worktree::file_diff(&t.repo, Side::Unstaged, "w.txt", None).unwrap();
    worktree::apply_selection(&t.repo, PatchOp::Stage, "w.txt", &[lines(&diff, 0, &["two", "TWO"])]).unwrap();
    assert_eq!(index_content(&t, "w.txt"), "one\r\nTWO\r\nthree\r\nfour\r\n");
}

#[test]
fn file_without_final_newline_can_be_staged_by_line() {
    let t = TestRepo::new();
    t.commit_file("f.txt", "one\ntwo\nthree", "base");
    t.write("f.txt", "ONE\ntwo\nthree\nfour");
    let diff = worktree::file_diff(&t.repo, Side::Unstaged, "f.txt", None).unwrap();
    worktree::apply_selection(&t.repo, PatchOp::Stage, "f.txt", &[lines(&diff, 0, &["one", "ONE"])]).unwrap();
    assert_eq!(index_content(&t, "f.txt"), "ONE\ntwo\nthree");
    let rest = worktree::file_diff(&t.repo, Side::Unstaged, "f.txt", None).unwrap();
    worktree::apply_selection(&t.repo, PatchOp::Stage, "f.txt", &[whole(&rest, 0)]).unwrap();
    assert_eq!(index_content(&t, "f.txt"), "ONE\ntwo\nthree\nfour");
}

#[test]
fn a_stale_selection_is_rejected_and_changes_nothing() {
    let t = TestRepo::new();
    let base = numbered(10);
    t.commit_file("f.txt", &base, "base");
    t.write("f.txt", &edit(&base, &[("line 5", "line five\n")]));
    let diff = worktree::file_diff(&t.repo, Side::Unstaged, "f.txt", None).unwrap();
    // The file changes after the UI loaded the diff.
    t.write("f.txt", &edit(&base, &[("line 1", "first\nsecond\n"), ("line 5", "line five\n")]));
    let err = worktree::apply_selection(&t.repo, PatchOp::Stage, "f.txt", &[whole(&diff, 0)]).unwrap_err();
    assert!(err.to_string().contains("changed since"), "{err}");
    assert_eq!(index_content(&t, "f.txt"), base);

    let empty = HunkSelection {
        index: 0,
        header: worktree::file_diff(&t.repo, Side::Unstaged, "f.txt", None).unwrap().hunks[0].header.clone(),
        lines: Some(vec![]),
    };
    let err = worktree::apply_selection(&t.repo, PatchOp::Stage, "f.txt", &[empty]).unwrap_err();
    assert!(err.to_string().contains("Select at least one"), "{err}");
    assert!(worktree::apply_selection(&t.repo, PatchOp::Stage, "missing.txt", &[]).is_err());
}

#[test]
fn binary_files_are_staged_whole() {
    let t = TestRepo::new();
    t.write_bytes("b.bin", &[0u8, 1, 2, 0]);
    t.stage_all();
    t.commit("base");
    t.write_bytes("b.bin", &[0u8, 9, 9, 0, 1]);
    let diff = worktree::file_diff(&t.repo, Side::Unstaged, "b.bin", None).unwrap();
    assert!(diff.binary);
    let sel = HunkSelection { index: 0, header: String::new(), lines: None };
    let err = worktree::apply_selection(&t.repo, PatchOp::Stage, "b.bin", &[sel]).unwrap_err();
    assert!(err.to_string().contains("binary"), "{err}");
    worktree::stage(&t.repo, &["b.bin".into()]).unwrap();
    assert_eq!(status_of(&t), (vec!["M b.bin".to_string()], vec![]));
}

fn commit_with(t: &TestRepo, has_git: bool, message: &str, amend: bool) -> anyhow::Result<worktree::CommitResult> {
    worktree::commit(&t.repo, &t.root(), has_git, &CommitRequest { message: message.into(), amend })
}

fn head_message(t: &TestRepo) -> String {
    let repo = git2::Repository::open(t.path()).unwrap();
    let c = repo.head().unwrap().peel_to_commit().unwrap();
    String::from_utf8_lossy(c.message_bytes()).into_owned()
}

fn commit_amend_and_undo(has_git: bool) {
    let t = TestRepo::new();
    t.write("a.txt", "one\n");
    worktree::stage(&t.repo, &["a.txt".into()]).unwrap();
    let first = commit_with(&t, has_git, "  First commit\n\nWith a body.\n\n", false).unwrap();
    assert_eq!(first.previous, None);
    assert_eq!(head_message(&t).trim_end(), "First commit\n\nWith a body.");

    t.write("a.txt", "one\ntwo\n");
    t.write("left-out.txt", "not staged\n");
    worktree::stage(&t.repo, &["a.txt".into()]).unwrap();
    let second = commit_with(&t, has_git, "Second commit", false).unwrap();
    assert_eq!(second.previous.as_deref(), Some(first.oid.as_str()));
    assert_eq!(status_of(&t), (vec![], vec!["? left-out.txt".to_string()]), "only staged changes are committed");

    // Amend: the commit is replaced, its parent stays.
    t.write("a.txt", "one\ntwo\nthree\n");
    worktree::stage(&t.repo, &["a.txt".into()]).unwrap();
    let amended = commit_with(&t, has_git, "Second commit, amended", true).unwrap();
    assert_ne!(amended.oid, second.oid);
    assert_eq!(amended.previous.as_deref(), Some(second.oid.as_str()));
    let repo = git2::Repository::open(t.path()).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.parent_id(0).unwrap().to_string(), first.oid);
    assert_eq!(head.message().unwrap().trim_end(), "Second commit, amended");

    // Undo the amend: the branch returns to the old commit, the new change stays staged.
    assert!(worktree::reset_soft(&repo, &second.oid, &first.oid).is_err(), "a wrong expected HEAD must not undo anything");
    worktree::reset_soft(&repo, &second.oid, &amended.oid).unwrap();
    assert_eq!(repo.head().unwrap().peel_to_commit().unwrap().id().to_string(), second.oid);
    assert_eq!(status_of(&t).0, vec!["M a.txt"]);
    assert_eq!(index_content(&t, "a.txt"), "one\ntwo\nthree\n");

    assert!(commit_with(&t, has_git, "   \n", false).is_err(), "an empty message is refused");
}

#[test]
fn commit_amend_and_undo_with_libgit2() {
    commit_amend_and_undo(false);
}

#[test]
fn commit_amend_and_undo_with_system_git() {
    if graftline::gitcli::detect().is_none() {
        eprintln!("skipped: git is not on PATH");
        return;
    }
    commit_amend_and_undo(true);
}

#[test]
fn commit_with_nothing_staged_fails_and_reports_the_git_message() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "one\n", "base");
    let err = commit_with(&t, false, "empty", false).unwrap_err();
    assert!(err.to_string().contains("Nothing is staged"), "{err}");
    if graftline::gitcli::detect().is_some() {
        let err = commit_with(&t, true, "empty", false).unwrap_err();
        let git = err.downcast_ref::<graftline::gitcli::GitError>().expect("a git failure keeps its command");
        assert_eq!(git.command, "git commit --cleanup=strip -F -");
        assert!(git.output.contains("nothing to commit"), "{}", git.output);
    }
    assert!(commit_with(&TestRepo::new(), false, "x", true).is_err(), "amend needs a commit");
}
