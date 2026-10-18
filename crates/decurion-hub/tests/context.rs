//! Files the owner attaches for agents: upload, list, download, remove, and
//! how a run and a helper get them.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::{router, store::Store, AppState, Keys};
use decurion_proto::context::MAX_FILE_BYTES;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";

struct Hub {
    r: axum::Router,
    dir: PathBuf,
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn state(store: Store) -> AppState {
    AppState::new(
        store,
        Keys {
            owner: OWNER.into(),
            runner: RUNNER.into(),
        },
        "abc123".into(),
        Some("https://pc.tail.ts.net:8443".into()),
    )
}

async fn hub() -> Hub {
    let dir = std::env::temp_dir().join(format!("decurion-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let r = router(state(store).with_files(dir.join("saved")));
    Hub { r, dir }
}

struct Resp {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    bytes: Vec<u8>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&self.bytes).into()))
    }
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

async fn send(
    r: &axum::Router,
    method: &str,
    path: &str,
    key: Option<&str>,
    content_type: &str,
    extra: &[(&str, &str)],
    body: Body,
) -> Resp {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, content_type);
    if let Some(k) = key {
        b = b.header(header::AUTHORIZATION, format!("Bearer {k}"));
    }
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let resp = r
        .clone()
        .oneshot(b.body(body).expect("request"))
        .await
        .expect("response");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    Resp {
        status,
        headers,
        bytes,
    }
}

async fn call(
    r: &axum::Router,
    method: &str,
    path: &str,
    key: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let body = if method == "GET" {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let resp = send(r, method, path, key, "application/json", &[], body).await;
    (resp.status, resp.json())
}

/// Attaches `bytes` as `name`, the way the page does: the file as the body,
/// with the file's own type.
async fn upload(
    r: &axum::Router,
    key: &str,
    app: i64,
    name: &str,
    query: &str,
    bytes: Vec<u8>,
) -> Resp {
    let name = name.replace(' ', "%20");
    send(
        r,
        "POST",
        &format!("/api/apps/{app}/files?name={name}{query}"),
        Some(key),
        "image/jpeg",
        &[],
        Body::from(bytes),
    )
    .await
}

async fn add_app(r: &axum::Router, name: &str) -> i64 {
    let (s, app) = call(
        r,
        "POST",
        "/api/apps",
        Some(OWNER),
        json!({"name": name, "repo_path": "C:\\repos\\demo", "profile": "A"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{app}");
    app["id"].as_i64().expect("id")
}

async fn add_goal(r: &axum::Router, app: i64) -> i64 {
    let (s, g) = call(
        r,
        "POST",
        "/api/goals",
        Some(OWNER),
        json!({"app_id": app, "text": "Go"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{g}");
    g["id"].as_i64().expect("goal")
}

/// A tiny Word file holding `text` in one paragraph.
fn docx(text: &str) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    w.start_file("word/document.xml", opts).expect("part");
    write!(
        w,
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"#
    )
    .expect("write");
    w.finish().expect("zip").into_inner()
}

/// The names in a tar, in order.
fn tar_names(t: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let mut at = 0;
    while at + 512 <= t.len() && t[at..at + 512].iter().any(|&b| b != 0) {
        let h = &t[at..at + 512];
        let end = h[..100].iter().position(|&b| b == 0).unwrap_or(100);
        names.push(String::from_utf8(h[..end].to_vec()).expect("name"));
        let size = std::str::from_utf8(&h[124..135]).expect("size");
        let size = usize::from_str_radix(size, 8).expect("octal");
        at += 512 + size.div_ceil(512) * 512;
    }
    names
}

async fn claim(r: &axum::Router, key: &str, name: &str) -> (StatusCode, Value) {
    call(
        r,
        "POST",
        "/api/runner/claim",
        Some(key),
        json!({"runner": name, "rules_version": decurion_proto::RULES_VERSION}),
    )
    .await
}

const PLAN: &str = r#"{"say": "Here is my plan.", "plan": {"summary": "A reset link.", "pieces": [{"title": "Reset link", "brief": "Add it.", "done_when": "It works."}]}}"#;

/// Signs up a cloud worker; returns its name and key.
async fn join(r: &axum::Router) -> (String, String) {
    let (_, j) = call(
        r,
        "POST",
        "/api/workers/join-code",
        Some(OWNER),
        Value::Null,
    )
    .await;
    let (s, reply) = call(r, "POST", "/runner/join", None, json!({"code": j["code"]})).await;
    assert_eq!(s, StatusCode::OK, "{reply}");
    (
        reply["name"].as_str().expect("name").to_owned(),
        reply["token"].as_str().expect("token").to_owned(),
    )
}

#[tokio::test]
async fn a_word_file_is_attached_listed_and_downloaded() {
    let h = hub().await;
    let app = add_app(&h.r, "demo").await;
    let bytes = docx("Prices go up in May.");
    let up = upload(
        &h.r,
        OWNER,
        app,
        "C:\\Users\\me\\Brief v2.docx",
        "",
        bytes.clone(),
    )
    .await;
    assert_eq!(up.status, StatusCode::CREATED, "{}", up.text());
    let f = up.json();
    assert_eq!(f["name"], "Brief v2.docx");
    assert_eq!(f["kind"], "word");
    assert_eq!(f["size"], bytes.len());
    assert_eq!(f["text_chars"], "Prices go up in May.".len());
    assert_eq!(f["note"], "");
    assert_eq!(f["goal_id"], Value::Null);
    let (s, list) = call(
        &h.r,
        "GET",
        &format!("/api/apps/{app}/files"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list.as_array().map(Vec::len), Some(1));
    assert_eq!(list[0]["id"], f["id"]);

    let id = f["id"].as_i64().expect("id");
    let d = send(
        &h.r,
        "GET",
        &format!("/api/apps/{app}/files/{id}"),
        Some(OWNER),
        "application/json",
        &[],
        Body::empty(),
    )
    .await;
    assert_eq!(d.status, StatusCode::OK);
    assert_eq!(d.bytes, bytes);
    let hdr = |k: header::HeaderName| {
        d.headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    assert_eq!(
        hdr(header::CONTENT_TYPE).as_deref(),
        Some("application/octet-stream")
    );
    assert_eq!(
        hdr(header::CONTENT_DISPOSITION).as_deref(),
        Some("attachment; filename=\"Brief v2.docx\"")
    );
    assert_eq!(
        hdr(header::X_CONTENT_TYPE_OPTIONS).as_deref(),
        Some("nosniff")
    );
    assert_eq!(
        hdr(header::CONTENT_SECURITY_POLICY).as_deref(),
        Some("sandbox")
    );
}

#[tokio::test]
async fn a_second_photo_with_the_same_name_gets_a_number() {
    let h = hub().await;
    let app = add_app(&h.r, "demo").await;
    let first = upload(&h.r, OWNER, app, "image.jpg", "", vec![1, 2, 3]).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.text());
    assert_eq!(first.json()["name"], "image.jpg");
    assert_eq!(first.json()["kind"], "image");
    let second = upload(&h.r, OWNER, app, "IMAGE.jpg", "", vec![4, 5]).await;
    assert_eq!(second.status, StatusCode::CREATED, "{}", second.text());
    assert_eq!(second.json()["name"], "IMAGE (2).jpg");
    let third = upload(&h.r, OWNER, app, "image.jpg", "", vec![6]).await;
    assert_eq!(third.json()["name"], "image (3).jpg");
}

#[tokio::test]
async fn bad_uploads_are_refused_in_plain_words() {
    let h = hub().await;
    let app = add_app(&h.r, "demo").await;
    let other = add_app(&h.r, "other").await;
    let goal = add_goal(&h.r, other).await;

    let r = upload(&h.r, OWNER, app, "a.pdf", &format!("&goal={goal}"), vec![1]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.text().contains("belongs to another app"), "{}", r.text());
    let r = upload(&h.r, OWNER, app, "a.pdf", "&goal=999", vec![1]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.text().contains("does not exist"), "{}", r.text());

    let r = upload(&h.r, OWNER, app, "a.pdf", "", Vec::new()).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.text().contains("empty"), "{}", r.text());
    let r = upload(&h.r, OWNER, app, "...", "", vec![1]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.text().contains("letters or numbers"), "{}", r.text());

    // Too large, read to the end of what is allowed.
    let big = vec![0u8; usize::try_from(MAX_FILE_BYTES).expect("fits") + 1];
    let r = upload(&h.r, OWNER, app, "big.pdf", "", big).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        r.text(),
        "This file is larger than 25 MB. Make it smaller, or split it, and try again."
    );
    // Too large as announced: refused before anything is read.
    let r = send(
        &h.r,
        "POST",
        &format!("/api/apps/{app}/files?name=big.pdf"),
        Some(OWNER),
        "application/pdf",
        &[("content-length", "999999999")],
        Body::from(vec![0u8; 10]),
    )
    .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(r.text().contains("larger than 25 MB"), "{}", r.text());

    let (_, list) = call(
        &h.r,
        "GET",
        &format!("/api/apps/{app}/files"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(list, json!([]));
    // Nothing half-written is left behind.
    let left: Vec<_> = std::fs::read_dir(h.dir.join("saved/context").join(format!("app-{app}")))
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
}

#[tokio::test]
async fn removing_a_file_deletes_it() {
    let h = hub().await;
    let app = add_app(&h.r, "demo").await;
    let f = upload(&h.r, OWNER, app, "Brief.docx", "", docx("Hello"))
        .await
        .json();
    let id = f["id"].as_i64().expect("id");
    let folder = h.dir.join("saved/context").join(format!("app-{app}"));
    assert!(folder.join(id.to_string()).is_file());
    assert!(folder.join(format!("{id}.txt")).is_file());
    let remove = format!("/api/apps/{app}/files/{id}/remove");
    let (s, _) = call(
        &h.r,
        "POST",
        &remove,
        Some(OWNER),
        json!({"receipt": "remove-receipt-1"}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(!folder.join(id.to_string()).exists());
    assert!(!folder.join(format!("{id}.txt")).exists());
    let (_, list) = call(
        &h.r,
        "GET",
        &format!("/api/apps/{app}/files"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(list, json!([]));
    // The same tap again does nothing again; without its receipt, it is gone.
    let (s, _) = call(
        &h.r,
        "POST",
        &remove,
        Some(OWNER),
        json!({"receipt": "remove-receipt-1"}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(&h.r, "POST", &remove, Some(OWNER), Value::Null).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_resent_upload_answers_with_the_first_file() {
    let h = hub().await;
    let app = add_app(&h.r, "demo").await;
    let q = "&receipt=upload-receipt-1";
    let first = upload(&h.r, OWNER, app, "image.jpg", q, vec![1, 2, 3]).await;
    assert_eq!(first.status, StatusCode::CREATED);
    let again = upload(&h.r, OWNER, app, "image.jpg", q, vec![1, 2, 3]).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text());
    assert_eq!(again.json(), first.json());
    let (_, list) = call(
        &h.r,
        "GET",
        &format!("/api/apps/{app}/files"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(list.as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn only_the_owner_attaches_sees_and_removes_files() {
    let h = hub().await;
    let app = add_app(&h.r, "demo").await;
    let f = upload(&h.r, OWNER, app, "a.txt", "", b"hi".to_vec())
        .await
        .json();
    let id = f["id"].as_i64().expect("id");
    let r = upload(&h.r, RUNNER, app, "b.txt", "", b"hi".to_vec()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    for (method, path) in [
        ("GET", format!("/api/apps/{app}/files")),
        ("GET", format!("/api/apps/{app}/files/{id}")),
        ("POST", format!("/api/apps/{app}/files/{id}/remove")),
    ] {
        let (s, _) = call(&h.r, method, &path, Some(RUNNER), Value::Null).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{method} {path}");
    }
    let (_, list) = call(
        &h.r,
        "GET",
        &format!("/api/apps/{app}/files"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(list.as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn a_hub_without_a_files_folder_says_so() {
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let r = router(state(store));
    let app = add_app(&r, "demo").await;
    let resp = upload(&r, OWNER, app, "a.txt", "", b"hi".to_vec()).await;
    assert_eq!(resp.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        resp.text().contains("cannot take attachments"),
        "{}",
        resp.text()
    );
}

#[tokio::test]
async fn the_files_panel_script_is_served_like_the_shared_script() {
    let h = hub().await;
    let r = send(
        &h.r,
        "GET",
        "/files.js",
        None,
        "text/plain",
        &[],
        Body::empty(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/javascript; charset=utf-8")
    );
    assert_eq!(
        r.headers
            .get(header::X_CONTENT_TYPE_OPTIONS)
            .and_then(|v| v.to_str().ok()),
        Some("nosniff")
    );
}

#[tokio::test]
async fn work_gets_its_apps_files_and_its_goals_but_not_another_goals() {
    let h = hub().await;
    let app = add_app(&h.r, "demo").await;
    let goal = add_goal(&h.r, app).await;
    let other_goal = add_goal(&h.r, app).await;
    for (name, query, bytes) in [
        ("Spec.pdf", String::new(), b"%PDF-1.4".to_vec()),
        ("Brief.docx", format!("&goal={goal}"), docx("For this goal")),
        (
            "Other.txt",
            format!("&goal={other_goal}"),
            b"not yours".to_vec(),
        ),
    ] {
        let r = upload(&h.r, OWNER, app, name, &query, bytes).await;
        assert_eq!(r.status, StatusCode::CREATED, "{name}: {}", r.text());
    }

    // The goal's coordinator: the app's files and its own goal's.
    let (s, job) = call(
        &h.r,
        "POST",
        "/api/runner/helper/claim",
        Some(RUNNER),
        json!({"runner": "local"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{job}");
    let names: Vec<&str> = job["context"]
        .as_array()
        .expect("context")
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    // The oldest goal's turn comes first.
    assert_eq!(job["source"]["goal_id"], goal, "{job}");
    assert_eq!(names, ["Brief.docx", "Spec.pdf"], "{job}");
    let id = job["job_id"].as_i64().expect("job");
    let t = send(
        &h.r,
        "GET",
        &format!("/api/runner/helper/{id}/context"),
        Some(RUNNER),
        "application/json",
        &[],
        Body::empty(),
    )
    .await;
    assert_eq!(t.status, StatusCode::OK, "{}", t.text());
    assert_eq!(
        tar_names(&t.bytes),
        ["Brief.docx", "Brief.docx.txt", "Spec.pdf"]
    );
    // A cloud worker never gets a helper's files.
    let (_, cloud) = join(&h.r).await;
    let (s, _) = call(
        &h.r,
        "GET",
        &format!("/api/runner/helper/{id}/context"),
        Some(&cloud),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    call(
        &h.r,
        "POST",
        &format!("/api/runner/helper/{id}/finish"),
        Some(RUNNER),
        json!({"ok": true, "text": PLAN}),
    )
    .await;

    // Approve the first goal's plan; its piece gets the app's and the goal's files.
    let (_, view) = call(
        &h.r,
        "GET",
        &format!("/api/goals/{goal}"),
        Some(OWNER),
        json!({}),
    )
    .await;
    assert_eq!(view["goal"]["state"], "plan_ready", "{view}");
    let plan = view["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["id"].as_i64())
        .expect("plan");
    let (s, v) = call(
        &h.r,
        "POST",
        &format!("/api/goals/{goal}/approve"),
        Some(OWNER),
        json!({"plan_message_id": plan}),
    )
    .await;
    assert!(s.is_success(), "{v}");
    let (s, work) = claim(&h.r, RUNNER, "local").await;
    assert_eq!(s, StatusCode::OK, "{work}");
    assert_eq!(
        work["context"],
        json!([
            {"name": "Brief.docx", "size": docx("For this goal").len(), "kind": "word", "text": true, "for_goal": true},
            {"name": "Spec.pdf", "size": 8, "kind": "pdf", "text": false, "for_goal": false},
        ])
    );
    let thread = work["thread"]["id"].as_i64().expect("thread");
    let t = send(
        &h.r,
        "GET",
        &format!("/api/runner/threads/{thread}/context"),
        Some(RUNNER),
        "application/json",
        &[],
        Body::empty(),
    )
    .await;
    assert_eq!(t.status, StatusCode::OK, "{}", t.text());
    assert_eq!(
        tar_names(&t.bytes),
        ["Brief.docx", "Brief.docx.txt", "Spec.pdf"]
    );
    // The copy built for sending is gone once sent.
    let runs: Vec<_> = std::fs::read_dir(h.dir.join("saved/context/runs"))
        .map(|d| d.flatten().collect())
        .unwrap_or_default();
    assert!(runs.is_empty(), "{runs:?}");

    // A cloud worker not running that work does not get its files.
    let (s, _) = call(
        &h.r,
        "GET",
        &format!("/api/runner/threads/{thread}/context"),
        Some(&cloud),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[tokio::test]
async fn work_outside_a_goal_gets_only_the_apps_files() {
    let h = hub().await;
    let app = add_app(&h.r, "demo").await;
    let goal = add_goal(&h.r, app).await;
    upload(&h.r, OWNER, app, "notes.md", "", b"# Notes".to_vec()).await;
    upload(
        &h.r,
        OWNER,
        app,
        "goal.md",
        &format!("&goal={goal}"),
        b"x".to_vec(),
    )
    .await;
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/threads",
        Some(OWNER),
        json!({"app_id": app, "title": "t", "prompt": "do it"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, work) = claim(&h.r, RUNNER, "local").await;
    assert_eq!(s, StatusCode::OK, "{work}");
    assert_eq!(
        work["context"],
        json!([{"name": "notes.md", "size": 7, "kind": "text", "text": false, "for_goal": false}])
    );
}
