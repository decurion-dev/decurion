//! Phase 2: the same work on the PC or a cloud worker, under the same rules.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::{router, store::Store, AppState, Keys};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";

struct Hub {
    r: axum::Router,
    store: Store,
    dir: PathBuf,
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn hub() -> Hub {
    let dir = std::env::temp_dir().join(format!("decurion-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let r = router(
        AppState::new(
            store.clone(),
            Keys {
                owner: OWNER.into(),
                runner: RUNNER.into(),
            },
            "abc123".into(),
            Some("https://pc.tail.ts.net:8443".into()),
        )
        .with_files(dir.join("saved")),
    );
    Hub { r, store, dir }
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
}

async fn send(r: &axum::Router, method: &str, path: &str, key: Option<&str>, body: Body) -> Resp {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(k) = key {
        b = b.header(header::AUTHORIZATION, format!("Bearer {k}"));
    }
    b = b.header(header::CONTENT_TYPE, "application/json");
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
    let resp = send(r, method, path, key, body).await;
    (resp.status, resp.json())
}

/// A real git repo standing in for the owner's app checkout.
fn app_repo(dir: &Path) -> PathBuf {
    let repo = dir.join("app");
    std::fs::create_dir_all(&repo).expect("repo dir");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "--quiet", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "hello\n").expect("write");
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "first"]);
    repo
}

async fn add_app(h: &Hub, profile: &str) -> i64 {
    let repo = app_repo(&h.dir);
    let (s, app) = call(
        &h.r,
        "POST",
        "/api/apps",
        Some(OWNER),
        json!({"name": format!("demo{profile}"), "repo_path": repo.to_string_lossy(), "profile": profile}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{app}");
    app["id"].as_i64().expect("id")
}

async fn add_work(h: &Hub, app: i64, place: &str) -> (StatusCode, Value) {
    call(
        &h.r,
        "POST",
        "/api/threads",
        Some(OWNER),
        json!({"app_id": app, "title": "t", "prompt": "do it", "place": place}),
    )
    .await
}

/// Signs up a cloud worker the way its setup text does; returns its key.
async fn join(h: &Hub) -> (String, String) {
    let (s, j) = call(
        &h.r,
        "POST",
        "/api/workers/join-code",
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    assert_eq!(j["hub_url"], "https://pc.tail.ts.net:8443");
    assert_eq!(j["minutes"], 1440);
    let code = j["code"].as_str().expect("code").to_owned();
    let (s, reply) = call(&h.r, "POST", "/runner/join", None, json!({ "code": code })).await;
    assert_eq!(s, StatusCode::OK, "{reply}");
    assert_eq!(reply["name"], j["name"]);
    // The code works once.
    let (s, _) = call(&h.r, "POST", "/runner/join", None, json!({ "code": code })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    (
        reply["name"].as_str().expect("name").to_owned(),
        reply["token"].as_str().expect("token").to_owned(),
    )
}

async fn claim(h: &Hub, key: &str, name: &str) -> (StatusCode, Value) {
    call(
        &h.r,
        "POST",
        "/api/runner/claim",
        Some(key),
        json!({ "runner": name, "rules_version": decurion_proto::RULES_VERSION }),
    )
    .await
}

#[tokio::test]
async fn moving_to_the_cloud_needs_a_cloud_worker() {
    let h = hub().await;
    let app = add_app(&h, "A").await;
    let (s, msg) = add_work(&h, app, "cloud").await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(msg.as_str().is_some_and(|m| m.contains("Add cloud worker")));
    let (_, t) = add_work(&h, app, "pc").await;
    let id = t["id"].as_i64().expect("id");
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/threads/{id}/place"),
        Some(OWNER),
        json!({"place":"cloud"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    join(&h).await;
    let (s, t) = call(
        &h.r,
        "POST",
        &format!("/api/threads/{id}/place"),
        Some(OWNER),
        json!({"place":"cloud"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t["place"], "cloud");
    let note: String = sqlx::query_scalar("SELECT note FROM audit WHERE thread_id = ?")
        .bind(id)
        .fetch_one(h.store.pool())
        .await
        .expect("audit");
    assert_eq!(note, "moved to the cloud");
    // Only the owner moves work.
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/threads/{id}/place"),
        Some(RUNNER),
        json!({"place":"pc"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn each_computer_only_picks_up_its_own_work() {
    let h = hub().await;
    let app = add_app(&h, "A").await;
    let (name, cloud) = join(&h).await;
    let (_, pc_work) = add_work(&h, app, "pc").await;
    let (_, cloud_work) = add_work(&h, app, "cloud").await;
    let (s, c) = claim(&h, &cloud, &name).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(c["thread"]["id"], cloud_work["id"]);
    assert_eq!(c["checkpoint"], Value::Null);
    let (s, c) = claim(&h, RUNNER, "local").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(c["thread"]["id"], pc_work["id"]);
    let (s, _) = claim(&h, RUNNER, "local").await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    // A cloud worker cannot pass itself off as the PC runner.
    let (s, _) = claim(&h, &cloud, "local").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_runner_only_touches_work_it_is_running() {
    let h = hub().await;
    let app = add_app(&h, "A").await;
    let (_, cloud) = join(&h).await;
    let (_, t) = add_work(&h, app, "pc").await;
    let id = t["id"].as_i64().expect("id");
    claim(&h, RUNNER, "local").await;
    let ev = json!({"kind":"message","text":"hi"});
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(&cloud),
        ev.clone(),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(&cloud),
        json!({"to":"failed"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    for path in ["base", "credential"] {
        let method = if path == "base" { "GET" } else { "POST" };
        let (s, _) = call(
            &h.r,
            method,
            &format!("/api/runner/threads/{id}/{path}"),
            Some(&cloud),
            Value::Null,
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT, "{path}");
    }
    let resp = send(
        &h.r,
        "PUT",
        &format!("/api/runner/threads/{id}/upload/code"),
        Some(&cloud),
        Body::from("x"),
    )
    .await;
    assert_eq!(resp.status, StatusCode::CONFLICT);
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(RUNNER),
        ev,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
}

#[tokio::test]
async fn a_runner_on_other_rules_is_turned_away() {
    let h = hub().await;
    let app = add_app(&h, "A").await;
    add_work(&h, app, "pc").await;
    claim(&h, RUNNER, "local").await;
    let (s, reply) = call(
        &h.r,
        "POST",
        "/api/runner/hello",
        Some(RUNNER),
        json!({"runner":"local","rules_version": decurion_proto::RULES_VERSION + 1}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(reply["rules_version"], decurion_proto::RULES_VERSION);
    assert!(reply["hub_commit"].is_string());
    // Its running work goes back to the queue rather than waiting on a
    // runner that must update first.
    let (_, t) = call(&h.r, "GET", "/api/threads", Some(OWNER), Value::Null).await;
    assert_eq!(t[0]["state"], "queued");
    // Claims are checked too, including from a runner that does not say.
    for body in [
        json!({"runner":"local"}),
        json!({"runner":"local","rules_version": decurion_proto::RULES_VERSION + 1}),
    ] {
        let (s, reply) = call(&h.r, "POST", "/api/runner/claim", Some(RUNNER), body).await;
        assert_eq!(s, StatusCode::CONFLICT);
        assert_eq!(reply["paused"], json!([]));
        assert_eq!(reply["rules_version"], decurion_proto::RULES_VERSION);
        assert!(reply["hub_commit"].is_string());
    }
    let (_, t) = call(&h.r, "GET", "/api/threads", Some(OWNER), Value::Null).await;
    assert_eq!(t[0]["state"], "queued");
}

#[tokio::test]
async fn a_cloud_worker_sees_only_the_work_it_runs() {
    let h = hub().await;
    let app = add_app(&h, "A").await;
    let (name, cloud) = join(&h).await;
    let (_, pc_work) = add_work(&h, app, "pc").await;
    let (_, cloud_work) = add_work(&h, app, "cloud").await;
    let pc_id = pc_work["id"].as_i64().expect("id");
    let cloud_id = cloud_work["id"].as_i64().expect("id");
    let events = format!("/api/threads/{cloud_id}/events");
    for path in [
        "/api/threads",
        "/api/apps",
        "/api/spend",
        "/api/workers",
        "/api/limits",
        "/api/devices",
        "/api/push/key",
        &events,
    ] {
        let (s, _) = call(&h.r, "GET", path, Some(&cloud), Value::Null).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{path}");
    }
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/threads/{cloud_id}/place"),
        Some(&cloud),
        json!({"place":"pc"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Work it is not running looks like work that does not exist.
    for id in [pc_id, cloud_id, cloud_id + 100] {
        let (s, body) = call(
            &h.r,
            "GET",
            &format!("/api/threads/{id}"),
            Some(&cloud),
            Value::Null,
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{id}");
        assert_eq!(body, "not found");
    }
    let (s, _) = claim(&h, &cloud, &name).await;
    assert_eq!(s, StatusCode::OK);
    let (s, t) = call(
        &h.r,
        "GET",
        &format!("/api/threads/{cloud_id}"),
        Some(&cloud),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t["id"], cloud_id);
    // The PC runner keeps its view of the board.
    let (s, _) = call(&h.r, "GET", "/api/threads", Some(RUNNER), Value::Null).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn work_from_before_saved_copies_stays_where_it_is() {
    let h = hub().await;
    let app = add_app(&h, "A").await;
    join(&h).await;
    let (_, t) = add_work(&h, app, "pc").await;
    let id = t["id"].as_i64().expect("id");
    claim(&h, RUNNER, "local").await;
    let path = format!("/api/threads/{id}/place");
    let place = |to: &'static str| call(&h.r, "POST", &path, Some(OWNER), json!({ "place": to }));
    // Running work can move: its runner saves a copy as it hands it over.
    let (s, _) = place("cloud").await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = place("pc").await;
    assert_eq!(s, StatusCode::OK);
    // Back in the queue with a branch and no saved copy, it cannot.
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/runner/hello",
        Some(RUNNER),
        json!({"runner":"local","rules_version": decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, msg) = place("cloud").await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(msg
        .as_str()
        .is_some_and(|m| m.contains("started before it could move")));
    let (_, t) = call(
        &h.r,
        "GET",
        &format!("/api/threads/{id}"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(t["place"], "pc");
}

#[tokio::test]
async fn work_saved_on_one_computer_continues_on_another() {
    let h = hub().await;
    let app = add_app(&h, "A").await;
    let (name, cloud) = join(&h).await;
    let (_, t) = add_work(&h, app, "pc").await;
    let id = t["id"].as_i64().expect("id");
    claim(&h, RUNNER, "local").await;

    // The PC runner gets the starting point from the hub.
    let base = send(
        &h.r,
        "GET",
        &format!("/api/runner/threads/{id}/base"),
        Some(RUNNER),
        Body::empty(),
    )
    .await;
    assert_eq!(base.status, StatusCode::OK);
    let sha = base.headers["x-decurion-base"]
        .to_str()
        .expect("sha")
        .to_owned();
    assert_eq!(sha.len(), 40);
    assert!(base.bytes.starts_with(b"# v2 git bundle"));
    let bundle = h.dir.join("base.bundle");
    std::fs::write(&bundle, &base.bytes).expect("write");
    let heads = std::process::Command::new("git")
        .args(["bundle", "list-heads"])
        .arg(&bundle)
        .output()
        .expect("git");
    assert_eq!(
        String::from_utf8_lossy(&heads.stdout).trim(),
        format!("{sha} refs/heads/decurion-base")
    );

    // A saved copy needs both parts.
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/runner/threads/{id}/checkpoint"),
        Some(RUNNER),
        json!({"head": sha, "base": sha}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    for (part, data) in [("code", "CODE BUNDLE"), ("session", "SESSION TAR")] {
        let r = send(
            &h.r,
            "PUT",
            &format!("/api/runner/threads/{id}/upload/{part}"),
            Some(RUNNER),
            Body::from(data),
        )
        .await;
        assert_eq!(r.status, StatusCode::NO_CONTENT);
    }
    let (s, cp) = call(
        &h.r,
        "POST",
        &format!("/api/runner/threads/{id}/checkpoint"),
        Some(RUNNER),
        json!({"head": sha, "base": sha}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{cp}");
    let cp = cp["id"].as_i64().expect("checkpoint id");

    // The owner moves it to the cloud; the PC runner hands it back.
    call(
        &h.r,
        "POST",
        &format!("/api/threads/{id}/place"),
        Some(OWNER),
        json!({"place":"cloud"}),
    )
    .await;
    let (_, t) = call(
        &h.r,
        "GET",
        &format!("/api/threads/{id}"),
        Some(RUNNER),
        Value::Null,
    )
    .await;
    assert_eq!(t["place"], "cloud");
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(RUNNER),
        json!({"to":"queued","note":"moved to the cloud"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, none) = claim(&h, RUNNER, "local").await;
    assert_eq!(none, Value::String(String::new()));

    // The cloud worker resumes the same session from the saved copy.
    let (s, c) = claim(&h, &cloud, &name).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(c["checkpoint"], cp);
    assert_eq!(c["resume"], false, "the agent never said anything yet");
    for (part, data) in [("code", "CODE BUNDLE"), ("session", "SESSION TAR")] {
        let r = send(
            &h.r,
            "GET",
            &format!("/api/runner/threads/{id}/checkpoint/{cp}/{part}"),
            Some(&cloud),
            Body::empty(),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(r.bytes, data.as_bytes());
    }
    let base2 = send(
        &h.r,
        "GET",
        &format!("/api/runner/threads/{id}/base"),
        Some(&cloud),
        Body::empty(),
    )
    .await;
    assert_eq!(base2.headers["x-decurion-base"], sha.as_str());
    assert_eq!(base2.bytes, base.bytes);
    // Nobody else's copies, and no made-up parts.
    let r = send(
        &h.r,
        "GET",
        &format!("/api/runner/threads/{}/checkpoint/{cp}/code", id + 1),
        Some(&cloud),
        Body::empty(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = send(
        &h.r,
        "GET",
        &format!("/api/runner/threads/{id}/checkpoint/{cp}/keys"),
        Some(&cloud),
        Body::empty(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = send(
        &h.r,
        "GET",
        &format!("/api/runner/threads/{id}/checkpoint/{cp}/code"),
        Some(OWNER),
        Body::empty(),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // While the cloud runs it, the PC may read its saved copies (to check
    // the work once it is done) but not change them.
    for path in [
        format!("/api/runner/threads/{id}/base"),
        format!("/api/runner/threads/{id}/checkpoint/{cp}/code"),
    ] {
        let r = send(&h.r, "GET", &path, Some(RUNNER), Body::empty()).await;
        assert_eq!(r.status, StatusCode::OK, "{path}");
    }
    let r = send(
        &h.r,
        "PUT",
        &format!("/api/runner/threads/{id}/upload/code"),
        Some(RUNNER),
        Body::from("PC CODE"),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/runner/threads/{id}/checkpoint"),
        Some(RUNNER),
        json!({"head": sha, "base": sha}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[tokio::test]
async fn only_the_newest_saved_copies_are_kept() {
    let h = hub().await;
    let app = add_app(&h, "A").await;
    let (_, t) = add_work(&h, app, "pc").await;
    let id = t["id"].as_i64().expect("id");
    claim(&h, RUNNER, "local").await;
    let head = "b".repeat(40);
    let mut ids = vec![];
    for i in 0..3 {
        for part in ["code", "session"] {
            send(
                &h.r,
                "PUT",
                &format!("/api/runner/threads/{id}/upload/{part}"),
                Some(RUNNER),
                Body::from(format!("{part}{i}")),
            )
            .await;
        }
        let (s, cp) = call(
            &h.r,
            "POST",
            &format!("/api/runner/threads/{id}/checkpoint"),
            Some(RUNNER),
            json!({ "head": head }),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        ids.push(cp["id"].as_i64().expect("id"));
    }
    let path = |cp: i64| format!("/api/runner/threads/{id}/checkpoint/{cp}/code");
    let gone = send(&h.r, "GET", &path(ids[0]), Some(RUNNER), Body::empty()).await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    let newest = send(&h.r, "GET", &path(ids[2]), Some(RUNNER), Body::empty()).await;
    assert_eq!(newest.bytes, b"code2");
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/runner/threads/{id}/checkpoint"),
        Some(RUNNER),
        json!({ "head": "HEAD" }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_cloud_worker_gets_only_the_credential_of_the_work_it_runs() {
    let h = hub().await;
    // Profile B's token, as Credential Manager would hold it on the PC.
    std::env::set_var("DECURION_CLAUDE_TOKEN_B", "sk-ant-oat-test-b");
    let app = add_app(&h, "B").await;
    let (name, cloud) = join(&h).await;
    let (_, t) = add_work(&h, app, "cloud").await;
    let id = t["id"].as_i64().expect("id");
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/runner/threads/{id}/credential"),
        Some(&cloud),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "not running yet");
    claim(&h, &cloud, &name).await;
    let (s, c) = call(
        &h.r,
        "POST",
        &format!("/api/runner/threads/{id}/credential"),
        Some(&cloud),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        c,
        json!({"var":"CLAUDE_CODE_OAUTH_TOKEN","value":"sk-ant-oat-test-b"})
    );
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/runner/threads/{id}/credential"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn removing_the_cloud_worker_brings_its_work_home() {
    let h = hub().await;
    let app = add_app(&h, "A").await;
    let (name, cloud) = join(&h).await;
    let (_, running) = add_work(&h, app, "cloud").await;
    let (_, waiting) = add_work(&h, app, "cloud").await;
    claim(&h, &cloud, &name).await;
    let (_, list) = call(&h.r, "GET", "/api/workers", Some(OWNER), Value::Null).await;
    assert_eq!(list[0]["name"], "local");
    assert_eq!(list[1]["name"], name.as_str());
    assert_eq!(list[1]["online"], true);
    let worker = list[1]["id"].as_i64().expect("id");
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/workers/{worker}/remove"),
        Some(RUNNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/workers/{worker}/remove"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    // Its key no longer works.
    let (s, _) = claim(&h, &cloud, &name).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (_, threads) = call(&h.r, "GET", "/api/threads", Some(OWNER), Value::Null).await;
    for t in threads.as_array().expect("list") {
        assert_eq!(t["place"], "pc");
        assert_eq!(t["state"], "queued");
    }
    assert_eq!(threads.as_array().map(Vec::len), Some(2));
    let _ = (running, waiting);
    let (_, list) = call(&h.r, "GET", "/api/workers", Some(OWNER), Value::Null).await;
    assert_eq!(list.as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn the_cloud_page_is_served_with_the_board_rules() {
    let h = hub().await;
    let r = send(&h.r, "GET", "/cloud", None, Body::empty()).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.headers[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .expect("csp")
        .contains("default-src 'self'"));
    let (s, _) = call(&h.r, "POST", "/runner/join", None, json!({"code":"nope"})).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/workers/join-code",
        Some(RUNNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}
