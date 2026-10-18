//! Dismissing finished work and closed goals (`POST /api/threads/dismiss`,
//! `POST /api/goals/dismiss`): it only hides them, and only the owner can.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::{router, store::Store, AppState, Keys};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";
const LOGIN: &str = "abc123def456";

async fn hub() -> (axum::Router, Store) {
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let r = router(AppState::new(
        store.clone(),
        Keys {
            owner: OWNER.into(),
            runner: RUNNER.into(),
        },
        LOGIN.into(),
        Some("https://hub.test".into()),
    ));
    (r, store)
}

async fn call(
    r: &axum::Router,
    method: &str,
    path: &str,
    key: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(k) = key {
        b = b.header(header::AUTHORIZATION, format!("Bearer {k}"));
    }
    let body = if method == "GET" {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let resp = r
        .clone()
        .oneshot(b.body(body).expect("request"))
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

async fn add_app(r: &axum::Router) -> i64 {
    let (s, app) = call(
        r,
        "POST",
        "/api/apps",
        Some(OWNER),
        json!({"name":"demo","repo_path":"C:\\repos\\demo","profile":"A"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{app}");
    app["id"].as_i64().expect("app id")
}

async fn add_work(r: &axum::Router, app: i64, title: &str) -> i64 {
    let (s, t) = call(
        r,
        "POST",
        "/api/threads",
        Some(OWNER),
        json!({"app_id": app, "title": title, "prompt": "do it"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{t}");
    t["id"].as_i64().expect("id")
}

async fn mv(r: &axum::Router, id: i64, key: &str, to: &str) {
    let (s, t) = call(
        r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(key),
        json!({ "to": to }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{t}");
}

/// Work the runner finished and the owner accepted.
async fn done_work(r: &axum::Router, app: i64, title: &str) -> i64 {
    let id = add_work(r, app, title).await;
    let (s, c) = call(
        r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["thread"]["id"], id);
    mv(r, id, RUNNER, "in_review").await;
    mv(r, id, OWNER, "done").await;
    id
}

async fn thread(r: &axum::Router, id: i64) -> Value {
    let (s, t) = call(
        r,
        "GET",
        &format!("/api/threads/{id}"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{t}");
    t
}

async fn dismiss(r: &axum::Router, what: &str, body: Value) -> (StatusCode, Value) {
    call(
        r,
        "POST",
        &format!("/api/{what}/dismiss"),
        Some(OWNER),
        body,
    )
    .await
}

#[tokio::test]
async fn finished_work_is_dismissed_and_brought_back_without_changing() {
    let (r, _) = hub().await;
    let app = add_app(&r).await;
    let a = done_work(&r, app, "Reset link").await;
    let b = done_work(&r, app, "Dark mode").await;
    let before = thread(&r, a).await;
    assert!(before["dismissed_at"].is_null());

    let (s, v) = dismiss(&r, "threads", json!({"ids": [a, b]})).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{v}");
    let after = thread(&r, a).await;
    let first = after["dismissed_at"]
        .as_str()
        .expect("dismissed")
        .to_owned();
    // Only hidden: its state, its time and its GitHub send stay as they were.
    assert_eq!(after["state"], "done");
    assert_eq!(after["updated_at"], before["updated_at"]);
    assert_eq!(after["github"], before["github"]);
    // Still listed, so the Dismissed view can show it.
    let (_, all) = call(&r, "GET", "/api/threads", Some(OWNER), Value::Null).await;
    assert_eq!(all.as_array().expect("list").len(), 2);

    // A resend keeps the first time.
    let (s, _) = dismiss(&r, "threads", json!({"ids": [a]})).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(thread(&r, a).await["dismissed_at"], first.as_str());

    let (s, _) = dismiss(&r, "threads", json!({"ids": [a], "dismissed": false})).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(thread(&r, a).await["dismissed_at"].is_null());
    assert!(thread(&r, b).await["dismissed_at"].is_string());
}

#[tokio::test]
async fn work_still_going_cannot_be_dismissed_and_nothing_half_happens() {
    let (r, _) = hub().await;
    let app = add_app(&r).await;
    let done = done_work(&r, app, "Reset link").await;
    let waiting = add_work(&r, app, "Dark mode").await;

    let (s, v) = dismiss(&r, "threads", json!({"ids": [done, waiting]})).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(v.as_str().expect("words").starts_with("Only finished work"));
    assert!(thread(&r, done).await["dismissed_at"].is_null());

    let (s, _) = dismiss(&r, "threads", json!({"ids": [9999]})).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, v) = dismiss(&r, "threads", json!({"ids": []})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v, "Pick something to dismiss first.");
}

#[tokio::test]
async fn retried_work_shows_again() {
    let (r, _) = hub().await;
    let app = add_app(&r).await;
    let id = add_work(&r, app, "Reset link").await;
    mv(&r, id, OWNER, "failed").await;
    let (s, _) = dismiss(&r, "threads", json!({"ids": [id]})).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(thread(&r, id).await["dismissed_at"].is_string());

    mv(&r, id, OWNER, "queued").await;
    assert!(thread(&r, id).await["dismissed_at"].is_null());
}

#[tokio::test]
async fn closed_goals_are_dismissed_and_open_ones_are_not() {
    let (r, _) = hub().await;
    let app = add_app(&r).await;
    let mut ids = Vec::new();
    for text in ["Add a reset link", "Add dark mode"] {
        let (s, g) = call(
            &r,
            "POST",
            "/api/goals",
            Some(OWNER),
            json!({"app_id": app, "text": text}),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{g}");
        ids.push(g["id"].as_i64().expect("goal id"));
    }
    let (open, stopped) = (ids[0], ids[1]);
    let (s, g) = call(
        &r,
        "POST",
        &format!("/api/goals/{stopped}/stop"),
        Some(OWNER),
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{g}");

    let (s, v) = dismiss(&r, "goals", json!({"ids": [open]})).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(v
        .as_str()
        .expect("words")
        .starts_with("Only finished goals"));

    let (s, _) = dismiss(&r, "goals", json!({"ids": [stopped]})).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, list) = call(&r, "GET", "/api/goals", Some(OWNER), Value::Null).await;
    let list = list.as_array().expect("goals");
    let find = |id: i64| list.iter().find(|g| g["id"] == id).expect("listed");
    assert!(find(stopped)["dismissed_at"].is_string());
    assert!(find(open)["dismissed_at"].is_null());

    let (s, _) = dismiss(&r, "goals", json!({"ids": [stopped], "dismissed": false})).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, g) = call(
        &r,
        "GET",
        &format!("/api/goals/{stopped}"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert!(g["goal"]["dismissed_at"].is_null());
}

#[tokio::test]
async fn only_the_owner_dismisses() {
    let (r, _) = hub().await;
    let app = add_app(&r).await;
    let id = done_work(&r, app, "Reset link").await;
    for what in ["threads", "goals"] {
        let (s, _) = call(
            &r,
            "POST",
            &format!("/api/{what}/dismiss"),
            Some(RUNNER),
            json!({"ids": [id]}),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{what}");
    }
    assert!(thread(&r, id).await["dismissed_at"].is_null());
}
