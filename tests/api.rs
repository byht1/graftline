//! The HTTP API as the UI uses it, against temporary repositories.

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use common::TestRepo;
use graftline::{router, Options};
use serde_json::{json, Value};
use tower::ServiceExt;

struct Api {
    app: axum::Router,
    _data: tempfile::TempDir,
}

impl Api {
    fn new(token: Option<&str>) -> Api {
        common::isolate_git_config();
        let data = tempfile::tempdir().unwrap();
        let (app, _) = router(Options { data_dir: data.path().to_path_buf(), app_token: token.map(str::to_string), open: Vec::new() });
        Api { app, _data: data }
    }

    async fn send(&self, req: Request<Body>) -> (StatusCode, Vec<(String, String)>, String) {
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string())).collect();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (status, headers, String::from_utf8_lossy(&body).into_owned())
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        let (status, _, body) = self.send(Request::get(path).body(Body::empty()).unwrap()).await;
        (status, serde_json::from_str(&body).unwrap_or(Value::String(body)))
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        let req = Request::post(path).header(header::CONTENT_TYPE, "application/json").body(Body::from(body.to_string())).unwrap();
        let (status, _, body) = self.send(req).await;
        (status, serde_json::from_str(&body).unwrap_or(Value::String(body)))
    }

    async fn open(&self, t: &TestRepo) -> String {
        let (status, v) = self.post("/api/repos/open", json!({ "path": t.path().display().to_string() })).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v["id"].as_str().unwrap().to_string()
    }
}

#[tokio::test]
async fn open_graph_commit_and_diff() {
    let t = TestRepo::new();
    let a = t.commit_file("a.txt", "one\n", "first commit");
    let b = t.commit_file("a.txt", "one\ntwo\n", "second commit");
    let api = Api::new(None);
    let id = api.open(&t).await;

    let (status, refs) = api.get(&format!("/api/repos/{id}/refs")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(refs["head"]["branch"], "main");
    assert_eq!(refs["branches"][0]["name"], "main");

    let (status, graph) = api.get(&format!("/api/repos/{id}/graph?offset=0&limit=50")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(graph["total"], 2);
    assert_eq!(graph["rows"][0]["o"], b.to_string());
    assert_eq!(graph["rows"][0]["s"], "second commit");
    assert_eq!(graph["rows"][0]["r"][0]["name"], "main");
    assert_eq!(graph["rows"][0]["p"], json!([1]));
    assert_eq!(graph["rows"][1]["o"], a.to_string());
    // Row 0 sends one line down (kind 2 = out), row 1 receives it (kind 1 = in).
    assert_eq!(graph["rows"][0]["e"], json!([2, 0, 0]));
    assert_eq!(graph["rows"][1]["e"], json!([1, 0, 0]));

    let (_, page) = api.get(&format!("/api/repos/{id}/graph?offset=1&limit=50")).await;
    assert_eq!(page["rows"].as_array().unwrap().len(), 1);
    assert_eq!(page["rows"][0]["o"], a.to_string());
    assert_eq!(page["version"], graph["version"]);

    let (_, found) = api.get(&format!("/api/repos/{id}/graph/search?q=second")).await;
    assert_eq!(found["rows"], json!([0]));
    let (_, located) = api.get(&format!("/api/repos/{id}/graph/locate?oids={a},{}", "0".repeat(40))).await;
    assert_eq!(located["rows"], json!([1, null]));

    let (status, commit) = api.get(&format!("/api/repos/{id}/commit/{b}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(commit["summary"], "second commit");
    assert_eq!(commit["files"][0]["path"], "a.txt");
    assert_eq!(commit["files"][0]["status"], "M");
    assert_eq!(commit["files"][0]["additions"], 1);

    let (status, diff) = api.get(&format!("/api/repos/{id}/diff?commit={b}&path=a.txt")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(diff["hunks"][0]["lines"][1]["t"], "+");
    assert_eq!(diff["hunks"][0]["lines"][1]["s"], "two");
    assert_eq!(diff["hunks"][0]["lines"][1]["n"], 2);
}

#[tokio::test]
async fn errors_are_reported_as_json() {
    let api = Api::new(None);
    let missing = std::env::temp_dir().join("graftline-definitely-missing-repo");
    let (status, v) = api.post("/api/repos/open", json!({ "path": missing.display().to_string() })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(v["error"].as_str().unwrap().contains("not inside a Git repository"), "{v}");

    let (status, v) = api.get("/api/repos/0000000000000000/graph").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(v["error"], "Repository not found");

    let t = TestRepo::new();
    t.commit_file("a.txt", "1", "first");
    let id = api.open(&t).await;
    let (status, v) = api.get(&format!("/api/repos/{id}/commit/not-a-hash")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(v["error"].as_str().unwrap().contains("not a commit id"));
}

#[tokio::test]
async fn recent_repositories_are_remembered_and_forgotten() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "1", "first");
    let api = Api::new(None);
    api.open(&t).await;
    api.open(&t).await;
    let (_, recent) = api.get("/api/recent").await;
    assert_eq!(recent.as_array().unwrap().len(), 1, "the same repository is listed once");
    assert_eq!(recent[0]["exists"], true);
    let path = recent[0]["path"].as_str().unwrap().to_string();
    let (status, _) = api.post("/api/recent/forget", json!({ "path": path })).await;
    assert_eq!(status, StatusCode::OK);
    let (_, recent) = api.get("/api/recent").await;
    assert!(recent.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn graph_changes_after_a_new_commit() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "1", "first");
    let api = Api::new(None);
    let id = api.open(&t).await;
    let (_, before) = api.get(&format!("/api/repos/{id}/graph")).await;
    assert_eq!(before["total"], 1);
    let b = t.commit_file("a.txt", "2", "made outside the app");
    // The watcher notices the new commit and rebuilds the history.
    let mut after = Value::Null;
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        after = api.get(&format!("/api/repos/{id}/graph")).await.1;
        if after["total"] == 2 {
            break;
        }
    }
    assert_eq!(after["total"], 2, "the graph did not pick up the new commit");
    assert_eq!(after["rows"][0]["o"], b.to_string());
    assert_ne!(after["version"], before["version"]);
}

#[tokio::test]
async fn desktop_token_is_required_and_becomes_a_cookie() {
    let api = Api::new(Some("s3cret"));
    let (status, _, _) = api.send(Request::get("/api/info").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = api.send(Request::get("/?token=wrong").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, headers, body) = api.send(Request::get("/?token=s3cret").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<div id=\"app\">"));
    let cookie = headers.iter().find(|(k, _)| k == "set-cookie").map(|(_, v)| v.clone()).expect("the token cookie is set");
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    let pair = cookie.split(';').next().unwrap().to_string();

    let (status, _, body) = api.send(Request::get("/api/info").header(header::COOKIE, pair).body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    let info: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(info["desktop"], true);
    assert_eq!(info["name"], "Graftline");
}

#[tokio::test]
async fn static_files_are_served() {
    let api = Api::new(None);
    for (path, mime) in [
        ("/style.css", "text/css"),
        ("/app.js", "text/javascript"),
        ("/graph.js", "text/javascript"),
        ("/brand/symbol.svg", "image/svg+xml"),
    ] {
        let (status, headers, body) = api.send(Request::get(path).body(Body::empty()).unwrap()).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        let ct = headers.iter().find(|(k, _)| k == "content-type").map(|(_, v)| v.as_str()).unwrap_or("");
        assert!(ct.starts_with(mime), "{path}: {ct}");
        assert!(!body.is_empty());
    }
    let (status, _, _) = api.send(Request::get("/no-such-file.js").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ui_preferences_round_trip() {
    let api = Api::new(None);
    let req = Request::put("/api/prefs")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "items": { "gl.theme": "\"nord\"", "other.key": "x" } }).to_string()))
        .unwrap();
    let (status, _, body) = api.send(req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, prefs) = api.get("/api/prefs").await;
    assert_eq!(prefs["rev"], 1);
    assert_eq!(prefs["items"], json!({ "gl.theme": "\"nord\"" }));
}

#[tokio::test]
async fn working_tree_endpoints_stage_commit_and_report_git_errors() {
    let t = TestRepo::new();
    t.commit_file("a.txt", "one\ntwo\nthree\n", "base");
    t.write("a.txt", "ONE\ntwo\nthree\nfour\n");
    t.write("new.txt", "new\n");
    let api = Api::new(None);
    let id = api.open(&t).await;

    let (_, status) = api.get(&format!("/api/repos/{id}/status")).await;
    assert_eq!(status["unstaged"].as_array().unwrap().len(), 2);
    assert!(status["staged"].as_array().unwrap().is_empty());
    // The working-tree row is the first row of the graph.
    let (_, graph) = api.get(&format!("/api/repos/{id}/graph")).await;
    assert_eq!(graph["rows"][0]["k"], "wip");
    assert_eq!(graph["rows"][0]["o"], "");
    assert_eq!(graph["total"], 2);

    let (_, diff) = api.get(&format!("/api/repos/{id}/diff?side=unstaged&path=a.txt")).await;
    let header = diff["hunks"][0]["header"].as_str().unwrap().to_string();
    let picked: Vec<usize> =
        diff["hunks"][0]["lines"].as_array().unwrap().iter().enumerate().filter(|(_, l)| l["s"] == "four").map(|(i, _)| i).collect();
    let (status_code, v) = api
        .post(
            &format!("/api/repos/{id}/apply"),
            json!({ "op": "stage", "path": "a.txt", "hunks": [{ "index": 0, "header": header, "lines": picked }] }),
        )
        .await;
    assert_eq!(status_code, StatusCode::OK, "{v}");
    let (_, staged) = api.get(&format!("/api/repos/{id}/diff?side=staged&path=a.txt")).await;
    let added: Vec<&str> =
        staged["hunks"][0]["lines"].as_array().unwrap().iter().filter(|l| l["t"] == "+").map(|l| l["s"].as_str().unwrap()).collect();
    assert_eq!(added, vec!["four"]);

    // A stale header is refused with a clear message.
    let (status_code, v) = api
        .post(
            &format!("/api/repos/{id}/apply"),
            json!({ "op": "stage", "path": "a.txt", "hunks": [{ "index": 0, "header": "@@ -9,9 +9,9 @@", "lines": null }] }),
        )
        .await;
    assert_eq!(status_code, StatusCode::BAD_REQUEST);
    assert!(v["error"].as_str().unwrap().contains("changed since"), "{v}");

    let (status_code, v) = api.post(&format!("/api/repos/{id}/commit"), json!({ "message": "Add line four" })).await;
    assert_eq!(status_code, StatusCode::OK, "{v}");
    let new_head = v["oid"].as_str().unwrap().to_string();
    assert_eq!(v["previous"].as_str().unwrap().len(), 40);
    let (_, graph) = api.get(&format!("/api/repos/{id}/graph")).await;
    assert_eq!(graph["rows"][1]["o"], new_head, "the graph shows the new commit without waiting for the file watch");

    // Discard returns the snapshot that can undo it.
    let (_, v) = api.post(&format!("/api/repos/{id}/discard"), json!({ "paths": ["a.txt", "new.txt"] })).await;
    let snapshot = v["snapshot"]["name"].as_str().expect("a snapshot name").to_string();
    assert_eq!(t.read("a.txt"), "one\ntwo\nthree\nfour\n");
    assert!(!t.path().join("new.txt").exists());
    let (_, list) = api.get(&format!("/api/repos/{id}/snapshots")).await;
    assert_eq!(list[0]["name"], snapshot);
    let (_, v) = api.post(&format!("/api/repos/{id}/snapshots/restore"), json!({ "name": snapshot })).await;
    assert_eq!(v["files"].as_array().unwrap().len(), 2);
    assert_eq!(t.read("a.txt"), "ONE\ntwo\nthree\nfour\n");
    assert_eq!(t.read("new.txt"), "new\n");

    // A failed git command comes back with the command and its output, word for word.
    if graftline::gitcli::detect().is_some() {
        api.post(&format!("/api/repos/{id}/discard"), json!({ "paths": ["a.txt", "new.txt"] })).await;
        let (status_code, v) = api.post(&format!("/api/repos/{id}/commit"), json!({ "message": "nothing here" })).await;
        assert_eq!(status_code, StatusCode::BAD_REQUEST);
        assert_eq!(v["command"], "git commit --cleanup=strip -F -");
        assert!(v["output"].as_str().unwrap().contains("nothing to commit"), "{v}");
    }
}
