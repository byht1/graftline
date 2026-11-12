//! The application state as the window uses it, against temporary repositories.

mod common;

use common::TestRepo;
use graftline::{gitcli, history, ops, repo, safety, worktree, AppState, Options, Shared};
use serde_json::{json, Value};
use std::sync::Arc;

struct App {
    st: Shared,
    _data: tempfile::TempDir,
}

impl App {
    fn new() -> App {
        common::isolate_git_config();
        let data = tempfile::tempdir().unwrap();
        App { st: AppState::new(Options { data_dir: data.path().to_path_buf(), desktop: false }), _data: data }
    }

    async fn open(&self, t: &TestRepo) -> String {
        self.st.open_repo(&t.path().display().to_string()).await.unwrap().id.clone()
    }

    async fn history(&self, id: &str) -> Arc<history::History> {
        self.st.session(id).await.unwrap().history().await.unwrap()
    }

    async fn diff(&self, id: &str, side: worktree::Side, path: &'static str) -> Value {
        serde_json::to_value(self.st.with_repo(id, move |r| worktree::file_diff(r, side, path, None)).await.unwrap()).unwrap()
    }
}

fn oid_at(h: &history::History, row: usize) -> String {
    h.commits[row].oid.to_string()
}

#[tokio::test]
async fn open_graph_commit_and_diff() {
    let t = TestRepo::new();
    let a = t.commit_file("a.txt", "one\n", "first commit");
    let b = t.commit_file("a.txt", "one\ntwo\n", "second commit");
    let app = App::new();
    let id = app.open(&t).await;

    let refs = serde_json::to_value(app.st.with_repo(&id, repo::refs).await.unwrap()).unwrap();
    assert_eq!(refs["head"]["branch"], "main");
    assert_eq!(refs["branches"][0]["name"], "main");

    let h = app.history(&id).await;
    assert_eq!(h.commits.len(), 2);
    assert_eq!(oid_at(&h, 0), b.to_string());
    assert_eq!(&*h.commits[0].summary, "second commit");
    assert_eq!(serde_json::to_value(h.labels.get(&b)).unwrap()[0]["name"], "main");
    assert_eq!(serde_json::to_value(&h.parents[0]).unwrap(), json!([1]));
    assert_eq!(oid_at(&h, 1), a.to_string());
    // Row 0 sends one line down (kind 2 = out), row 1 receives it (kind 1 = in).
    let rows = h.layout.rows(0, 2);
    let edges = |i: usize| -> Vec<u16> { rows[i].edges.iter().flat_map(|e| [e.kind as u16, e.col, e.color as u16]).collect() };
    assert_eq!(edges(0), vec![2, 0, 0]);
    assert_eq!(edges(1), vec![1, 0, 0]);

    assert_eq!(h.search("second"), vec![0]);
    assert_eq!(h.index.get(&a).copied(), Some(1));
    assert_eq!(h.index.get(&git2::Oid::from_str(&"0".repeat(40)).unwrap()), None);

    let commit = serde_json::to_value(app.st.with_repo(&id, move |r| repo::commit_detail(r, b)).await.unwrap()).unwrap();
    assert_eq!(commit["summary"], "second commit");
    assert_eq!(commit["files"][0]["path"], "a.txt");
    assert_eq!(commit["files"][0]["status"], "M");
    assert_eq!(commit["files"][0]["additions"], 1);

    let diff = serde_json::to_value(app.st.with_repo(&id, move |r| repo::commit_file_diff(r, b, "a.txt", None)).await.unwrap()).unwrap();
    assert_eq!(diff["hunks"][0]["lines"][1]["t"], "+");
    assert_eq!(diff["hunks"][0]["lines"][1]["s"], "two");
    assert_eq!(diff["hunks"][0]["lines"][1]["n"], 2);
}

#[tokio::test]
async fn errors_are_reported() {
    let app = App::new();
    let missing = std::env::temp_dir().join("graftline-definitely-missing-repo");
    let e = app.st.open_repo(&missing.display().to_string()).await.err().unwrap();
    assert!(format!("{e:#}").contains("not inside a Git repository"), "{e:#}");

    let e = app.st.session("0000000000000000").await.err().unwrap();
    assert_eq!(e.to_string(), "Repository not found");

    let t = TestRepo::new();
    t.commit_file("a.txt", "1", "first");
    let id = app.open(&t).await;
    let e = app.st.with_repo(&id, |r| repo::commit_detail(r, repo::parse_oid("not-a-hash")?)).await.err().unwrap();
    assert!(format!("{e:#}").contains("not a commit id"));
}

#[tokio::test]
async fn recent_repositories_are_remembered_and_forgotten() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "1", "first");
    let app = App::new();
    app.open(&t).await;
    app.open(&t).await;
    let recent = app.st.recent().await;
    assert_eq!(recent.len(), 1, "the same repository is listed once");
    assert!(recent[0].1);
    app.st.forget_recent(&recent[0].0.path).await.unwrap();
    assert!(app.st.recent().await.is_empty());
}

#[tokio::test]
async fn graph_changes_after_a_new_commit() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "1", "first");
    let app = App::new();
    let id = app.open(&t).await;
    let before = app.history(&id).await;
    assert_eq!(before.commits.len(), 1);
    let b = t.commit_file("a.txt", "2", "made outside the app");
    // The watcher notices the new commit and rebuilds the history.
    let mut after = before.clone();
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        after = app.history(&id).await;
        if after.commits.len() == 2 {
            break;
        }
    }
    assert_eq!(after.commits.len(), 2, "the graph did not pick up the new commit");
    assert_eq!(oid_at(&after, 0), b.to_string());
    assert_ne!(after.fingerprint, before.fingerprint);
}

#[tokio::test]
async fn ui_preferences_round_trip() {
    let app = App::new();
    app.st.set_pref("gl.theme", Some("\"nord\"".into())).await.unwrap();
    app.st.set_pref("other.key", Some("x".into())).await.unwrap();
    let prefs = app.st.prefs().await;
    assert_eq!(serde_json::to_value(&prefs).unwrap(), json!({ "gl.theme": "\"nord\"" }));
    // A new state reads the file that the first one wrote.
    let again = AppState::new(Options { data_dir: app.st.data_dir.clone(), desktop: false });
    assert_eq!(again.prefs().await, prefs);
    app.st.set_pref("gl.theme", None).await.unwrap();
    assert!(app.st.prefs().await.is_empty());
}

#[tokio::test]
async fn working_tree_calls_stage_commit_and_report_git_errors() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "one\ntwo\nthree\n", "base");
    t.write("a.txt", "ONE\ntwo\nthree\nfour\n");
    t.write("new.txt", "new\n");
    let app = App::new();
    let id = app.open(&t).await;

    let status = serde_json::to_value(app.st.with_repo(&id, worktree::status).await.unwrap()).unwrap();
    assert_eq!(status["unstaged"].as_array().unwrap().len(), 2);
    assert!(status["staged"].as_array().unwrap().is_empty());
    // The working-tree row is the first row of the graph.
    let h = app.history(&id).await;
    assert_eq!(h.commits[0].kind, history::Kind::Wip);
    assert_eq!(h.commits.len(), 2);

    let diff = app.diff(&id, worktree::Side::Unstaged, "a.txt").await;
    let header = diff["hunks"][0]["header"].as_str().unwrap().to_string();
    let picked: Vec<usize> =
        diff["hunks"][0]["lines"].as_array().unwrap().iter().enumerate().filter(|(_, l)| l["s"] == "four").map(|(i, _)| i).collect();
    let hunks: Vec<worktree::HunkSelection> = serde_json::from_value(json!([{ "index": 0, "header": header, "lines": picked }])).unwrap();
    app.st.mutate(&id, move |r, _| worktree::apply_selection(r, worktree::PatchOp::Stage, "a.txt", &hunks)).await.unwrap();
    let staged = app.diff(&id, worktree::Side::Staged, "a.txt").await;
    let added: Vec<&str> =
        staged["hunks"][0]["lines"].as_array().unwrap().iter().filter(|l| l["t"] == "+").map(|l| l["s"].as_str().unwrap()).collect();
    assert_eq!(added, vec!["four"]);

    // A stale header is refused with a clear message.
    let stale: Vec<worktree::HunkSelection> =
        serde_json::from_value(json!([{ "index": 0, "header": "@@ -9,9 +9,9 @@", "lines": null }])).unwrap();
    let e = app.st.mutate(&id, move |r, _| worktree::apply_selection(r, worktree::PatchOp::Stage, "a.txt", &stale)).await.err().unwrap();
    assert!(format!("{e:#}").contains("changed since"), "{e:#}");

    let has_git = app.st.git.is_some();
    let req: worktree::CommitRequest = serde_json::from_value(json!({ "message": "Add line four" })).unwrap();
    let done = serde_json::to_value(app.st.mutate(&id, move |r, s| worktree::commit(r, &s.root, has_git, &req)).await.unwrap()).unwrap();
    let new_head = done["oid"].as_str().unwrap().to_string();
    assert_eq!(done["previous"].as_str().unwrap().len(), 40);
    let h = app.history(&id).await;
    assert_eq!(oid_at(&h, 1), new_head, "the graph shows the new commit without waiting for the file watch");

    // Discard returns the snapshot that can undo it.
    let paths = vec!["a.txt".to_string(), "new.txt".to_string()];
    let p = paths.clone();
    let snap = serde_json::to_value(app.st.mutate(&id, move |r, _| worktree::discard(r, &p)).await.unwrap()).unwrap();
    let snapshot = snap["name"].as_str().expect("a snapshot name").to_string();
    assert_eq!(t.read("a.txt"), "one\ntwo\nthree\nfour\n");
    assert!(!t.path().join("new.txt").exists());
    let list = serde_json::to_value(app.st.with_repo(&id, safety::list).await.unwrap()).unwrap();
    assert_eq!(list[0]["name"], snapshot);
    let files = app.st.mutate(&id, move |r, _| safety::restore(r, &snapshot)).await.unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(t.read("a.txt"), "ONE\ntwo\nthree\nfour\n");
    assert_eq!(t.read("new.txt"), "new\n");

    // A failed git command comes back with the command and its output, word for word.
    if has_git {
        app.st.mutate(&id, move |r, _| worktree::discard(r, &paths)).await.unwrap();
        let req: worktree::CommitRequest = serde_json::from_value(json!({ "message": "nothing here" })).unwrap();
        let e = app.st.mutate(&id, move |r, s| worktree::commit(r, &s.root, true, &req)).await.err().unwrap();
        let git = e.downcast_ref::<gitcli::GitError>().expect("a git error");
        assert_eq!(git.command, "git commit --cleanup=strip -F -");
        assert!(git.output.contains("nothing to commit"), "{}", git.output);
    }
}

#[tokio::test]
async fn operations_and_clone_requests_are_validated() {
    let t = TestRepo::new();
    let a = t.commit_file("a.txt", "1", "first");
    let app = App::new();
    let id = app.open(&t).await;

    let op: ops::Op = serde_json::from_value(json!({ "op": "branch-create", "name": "topic", "at": a.to_string() })).unwrap();
    app.st.mutate(&id, move |r, s| ops::run(r, &s.root, op)).await.unwrap();
    let refs = app.st.with_repo(&id, repo::refs).await.unwrap();
    let names: Vec<&str> = refs.branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, vec!["main", "topic"]);
    // The graph already shows the new label: the operation refreshed the session.
    let h = app.history(&id).await;
    let labels = serde_json::to_value(h.labels.get(&a)).unwrap();
    let labels: Vec<&str> = labels.as_array().unwrap().iter().map(|l| l["name"].as_str().unwrap()).collect();
    assert_eq!(labels, vec!["main", "topic"]);

    let op: ops::Op = serde_json::from_value(json!({ "op": "branch-create", "name": "topic" })).unwrap();
    let e = app.st.mutate(&id, move |r, s| ops::run(r, &s.root, op)).await.err().unwrap();
    assert!(format!("{e:#}").contains("already exists"));

    let parent = t.path().display().to_string();
    let e = app.st.clone_repo("", &parent, None).err().unwrap();
    assert!(e.to_string().contains("URL"));
    let e = app.st.clone_repo("https://example.com/x.git", "/no/such/folder/anywhere", None).err().unwrap();
    assert!(e.to_string().contains("not a folder"));
    let e = app.st.clone_repo("https://example.com/x.git", &parent, Some("../escape")).err().unwrap();
    assert!(e.to_string().contains("not a folder name"), "{e}");
    assert!(app.st.jobs.list().is_empty());
}
