//! Removing an app from Decurion: it hides, keeps history, frees the name and
//! folder, and is refused while work is going. Made-up apps only.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::{router, store::Store, AppState, Keys};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";

async fn hub() -> axum::Router {
    let store = Store::open("sqlite::memory:").await.expect("open store");
    router(AppState::new(
        store,
        Keys {
            owner: OWNER.into(),
            runner: RUNNER.into(),
        },
        "abc123".into(),
        None,
    ))
}

async fn call(
    r: &axum::Router,
    method: &str,
    path: &str,
    key: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .body(if method == "GET" {
            Body::empty()
        } else {
            Body::from(body.to_string())
        })
        .expect("request");
    let resp = r.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

async fn add_app(r: &axum::Router, name: &str, pay_per_use: bool) -> (StatusCode, Value) {
    call(
        r,
        "POST",
        "/api/apps",
        OWNER,
        json!({"name": name, "repo_path": "C:\\repos\\portal", "profile": "A", "pay_per_use": pay_per_use}),
    )
    .await
}

async fn app(r: &axum::Router, name: &str) -> i64 {
    let (s, v) = add_app(r, name, false).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    v["id"].as_i64().expect("app id")
}

async fn names(r: &axum::Router) -> Vec<String> {
    let (s, v) = call(r, "GET", "/api/apps", OWNER, Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    v.as_array()
        .expect("list")
        .iter()
        .map(|a| a["name"].as_str().expect("name").to_owned())
        .collect()
}

async fn remove(r: &axum::Router, id: i64, receipt: &str) -> (StatusCode, Value) {
    call(
        r,
        "POST",
        &format!("/api/apps/{id}/remove"),
        OWNER,
        json!({"receipt": receipt}),
    )
    .await
}

async fn work(r: &axum::Router, app: i64) -> i64 {
    let (s, t) = call(
        r,
        "POST",
        "/api/threads",
        OWNER,
        json!({"app_id": app, "title": "t", "prompt": "p"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{t}");
    t["id"].as_i64().expect("thread id")
}

#[tokio::test]
async fn removing_an_idle_app_hides_it_everywhere() {
    let r = hub().await;
    let id = app(&r, "portal").await;
    app(&r, "demo").await;
    let (s, _) = remove(&r, id, "remove-0001").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(names(&r).await, vec!["demo"]);
    // Old links to it answer "not found".
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/apps/{id}/edit"),
        OWNER,
        json!({"name": "x"}),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &r,
        "POST",
        "/api/threads",
        OWNER,
        json!({"app_id": id, "title": "t", "prompt": "p"}),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Removing it again is harmless.
    let (s, _) = remove(&r, id, "remove-0002").await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn an_app_with_unfinished_work_is_refused_and_nothing_changes() {
    let r = hub().await;
    let id = app(&r, "portal").await;
    work(&r, id).await;
    let (s, v) = remove(&r, id, "remove-0001").await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(
        v.to_string()
            .contains("portal still has work going. Finish or stop it first."),
        "{v}"
    );
    assert_eq!(names(&r).await, vec!["portal"]);
}

#[tokio::test]
async fn the_same_name_and_folder_can_be_added_after_removal() {
    let r = hub().await;
    let id = app(&r, "portal").await;
    let (s, _) = remove(&r, id, "remove-0001").await;
    assert_eq!(s, StatusCode::OK);
    let again = app(&r, "portal").await;
    assert_ne!(again, id);
    assert_eq!(names(&r).await, vec!["portal"]);
    // The new app is checked as strictly as ever.
    let (s, _) = add_app(&r, "portal~removed-1", false).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = add_app(&r, "portal", false).await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_resent_receipt_gets_the_first_result() {
    let r = hub().await;
    let id = app(&r, "portal").await;
    let first = remove(&r, id, "remove-0001").await;
    assert_eq!(first.0, StatusCode::OK);
    // The name is added again; a resend must not remove the new app.
    let again = app(&r, "portal").await;
    let resend = remove(&r, id, "remove-0001").await;
    assert_eq!(resend, first);
    assert_eq!(names(&r).await, vec!["portal"]);
    // The same receipt for another app is refused, not guessed at.
    let (s, _) = remove(&r, again, "remove-0001").await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(names(&r).await, vec!["portal"]);
}

#[tokio::test]
async fn a_runner_key_cannot_remove_an_app() {
    let r = hub().await;
    let id = app(&r, "portal").await;
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/apps/{id}/remove"),
        RUNNER,
        json!({"receipt": "remove-0001"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(names(&r).await, vec!["portal"]);
}

#[tokio::test]
async fn a_removed_apps_past_spend_still_counts() {
    let r = hub().await;
    let id = {
        let (s, v) = add_app(&r, "portal", true).await;
        assert_eq!(s, StatusCode::CREATED);
        v["id"].as_i64().expect("id")
    };
    let t = work(&r, id).await;
    let (s, c) = call(
        &r,
        "POST",
        "/api/runner/claim",
        RUNNER,
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    for body in [
        json!({"kind":"message","text":"working"}),
        json!({"kind":"finished","ok":true,"summary":"done","cost_usd":3.25}),
    ] {
        call(
            &r,
            "POST",
            &format!("/api/threads/{t}/events"),
            RUNNER,
            body,
        )
        .await;
    }
    for (key, to) in [(RUNNER, "in_review"), (OWNER, "done")] {
        let (s, v) = call(
            &r,
            "POST",
            &format!("/api/threads/{t}/move"),
            key,
            json!({"to": to}),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    let (_, before) = call(&r, "GET", "/api/spend", OWNER, Value::Null).await;
    assert_eq!(before["pay_per_use_usd"], 3.25);
    let (s, v) = remove(&r, id, "remove-0001").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, after) = call(&r, "GET", "/api/spend", OWNER, Value::Null).await;
    assert_eq!(after["pay_per_use_usd"], 3.25);
    assert_eq!(after["apps"], json!([]));
}
