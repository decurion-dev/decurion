//! What the screens show about work that is alive: a piece's check while it
//! waits and runs, helper agents for the board's Running and Queued counts,
//! and a running piece's newest note and last activity (live.rs).

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::{router, store::Store, AppState, Keys};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";

async fn hub() -> (axum::Router, Store) {
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let r = router(AppState::new(
        store.clone(),
        Keys {
            owner: OWNER.into(),
            runner: RUNNER.into(),
        },
        "abc123".into(),
        None,
    ));
    (r, store)
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

async fn ok(r: &axum::Router, method: &str, path: &str, key: &str, body: Value) -> Value {
    let (s, v) = call(r, method, path, key, body).await;
    assert!(s.is_success(), "{method} {path}: {s} {v}");
    v
}

async fn get(r: &axum::Router, path: &str) -> Value {
    ok(r, "GET", path, OWNER, json!({})).await
}

async fn app(r: &axum::Router) -> i64 {
    let v = ok(
        r,
        "POST",
        "/api/apps",
        OWNER,
        json!({"name":"portal","repo_path":"C:\\repos\\portal","profile":"A"}),
    )
    .await;
    v["id"].as_i64().expect("app id")
}

/// Claims the next helper job and says its container started.
async fn claim_helper(r: &axum::Router, kind: &str) -> i64 {
    let claim = ok(
        r,
        "POST",
        "/api/runner/helper/claim",
        RUNNER,
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(claim["kind"], kind, "{claim}");
    let id = claim["job_id"].as_i64().expect("job id");
    ok(
        r,
        "POST",
        &format!("/api/runner/helper/{id}/started"),
        RUNNER,
        json!({}),
    )
    .await;
    id
}

async fn finish_helper(r: &axum::Router, id: i64, text: &str) {
    ok(
        r,
        "POST",
        &format!("/api/runner/helper/{id}/finish"),
        RUNNER,
        json!({"ok": true, "text": text, "cost_usd": 0.02}),
    )
    .await;
}

const PLAN: &str = r#"```json
{"say": "Here is my plan.", "plan": {"summary": "A forgot-password link.", "pieces": [{"title": "Reset link", "brief": "Add a reset flow.", "done_when": "You can reset your password."}]}}
```"#;

/// One approved piece whose agent finished, so its check is lined up.
async fn piece_in_review(r: &axum::Router) -> (i64, i64) {
    let app = app(r).await;
    let g = ok(
        r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "Go"}),
    )
    .await;
    let goal = g["id"].as_i64().expect("goal");
    let lead = claim_helper(r, "coordinator").await;
    finish_helper(r, lead, PLAN).await;
    let v = get(r, &format!("/api/goals/{goal}")).await;
    let plan_id = v["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["id"].as_i64())
        .expect("plan");
    ok(
        r,
        "POST",
        &format!("/api/goals/{goal}/approve"),
        OWNER,
        json!({"plan_message_id": plan_id}),
    )
    .await;
    let design = claim_helper(r, "design").await;
    finish_helper(r, design, "done").await;
    let work = ok(
        r,
        "POST",
        "/api/runner/claim",
        RUNNER,
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    let thread = work["thread"]["id"].as_i64().expect("thread");
    ok(
        r,
        "POST",
        &format!("/api/threads/{thread}/move"),
        RUNNER,
        json!({"to":"in_review"}),
    )
    .await;
    (goal, thread)
}

fn piece(v: &Value) -> &Value {
    &v["pieces"][0]
}

#[tokio::test]
async fn a_pieces_check_shows_while_it_is_lined_up_and_while_it_runs() {
    let (r, _) = hub().await;
    let (goal, thread) = piece_in_review(&r).await;

    // Lined up: the piece, its goal row and the board's helpers all say so.
    let t = get(&r, &format!("/api/threads/{thread}")).await;
    assert_eq!(t["state"], "in_review");
    assert_eq!(t["check"], json!({"state": "queued"}), "{t}");
    let listed = get(&r, "/api/threads").await;
    assert_eq!(listed[0]["check"]["state"], "queued");
    assert_eq!(
        piece(&get(&r, &format!("/api/goals/{goal}")).await)["check"]["state"],
        "queued"
    );
    let helpers = get(&r, "/api/helpers").await;
    assert_eq!(helpers.as_array().map(Vec::len), Some(1), "{helpers}");
    assert_eq!(helpers[0]["kind"], "checker");
    assert_eq!(helpers[0]["state"], "queued");
    assert_eq!(helpers[0]["thread_id"], thread);
    assert_eq!(helpers[0]["piece_title"], "Reset link");

    // Running: when it started.
    let job = claim_helper(&r, "review").await;
    let t = get(&r, &format!("/api/threads/{thread}")).await;
    assert_eq!(t["check"]["state"], "running");
    assert!(t["check"]["started_at"].is_string(), "{t}");
    let helpers = get(&r, "/api/helpers").await;
    assert_eq!(helpers[0]["state"], "running");
    assert_eq!(helpers[0]["started_at"], t["check"]["started_at"]);

    // Finished: no check is coming, and no helper is at work.
    finish_helper(
        &r,
        job,
        r#"{"pass": false, "summary": "Broken.", "issues": ["The link 404s."]}"#,
    )
    .await;
    let t = get(&r, &format!("/api/threads/{thread}")).await;
    assert!(t.get("check").is_none(), "{t}");
    assert!(piece(&get(&r, &format!("/api/goals/{goal}")).await)
        .get("check")
        .is_none());
}

#[tokio::test]
async fn a_check_held_up_by_a_max_plan_that_is_out_says_why() {
    let (r, store) = hub().await;
    let (goal, thread) = piece_in_review(&r).await;
    sqlx::query(
        "INSERT INTO profile_limits (profile, until) VALUES ('A', strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+1 hour'))",
    )
    .execute(store.pool())
    .await
    .expect("limit");
    let t = get(&r, &format!("/api/threads/{thread}")).await;
    assert_eq!(t["check"]["state"], "queued");
    assert_eq!(
        t["check"]["waiting"],
        "Claude Max plan A is out of usage for now."
    );
    assert!(t["check"]["until"].is_string(), "{t}");
    let helpers = get(&r, "/api/helpers").await;
    assert_eq!(helpers[0]["waiting"], t["check"]["waiting"]);
    assert_eq!(
        piece(&get(&r, &format!("/api/goals/{goal}")).await)["check"]["waiting"],
        t["check"]["waiting"]
    );

    // Once the owner chose pay-per-use for this outage, it only waits for an agent.
    ok(&r, "POST", "/api/limits/A/pay-per-use", OWNER, json!({})).await;
    let t = get(&r, &format!("/api/threads/{thread}")).await;
    assert_eq!(t["check"], json!({"state": "queued"}), "{t}");
}

#[tokio::test]
async fn a_stopped_goal_leaves_no_check_showing() {
    let (r, _) = hub().await;
    let (goal, thread) = piece_in_review(&r).await;
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/stop"),
        OWNER,
        json!({}),
    )
    .await;
    let t = get(&r, &format!("/api/threads/{thread}")).await;
    assert!(t.get("check").is_none(), "{t}");
    assert_eq!(get(&r, "/api/helpers").await, json!([]));
}

#[tokio::test]
async fn work_added_straight_to_the_board_has_no_check() {
    let (r, _) = hub().await;
    let app = app(&r).await;
    let t = ok(
        &r,
        "POST",
        "/api/threads",
        OWNER,
        json!({"app_id": app, "title": "t", "prompt": "p"}),
    )
    .await;
    let id = t["id"].as_i64().expect("id");
    ok(
        &r,
        "POST",
        "/api/runner/claim",
        RUNNER,
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    ok(
        &r,
        "POST",
        &format!("/api/threads/{id}/move"),
        RUNNER,
        json!({"to":"in_review"}),
    )
    .await;
    let t = get(&r, &format!("/api/threads/{id}")).await;
    assert!(t.get("check").is_none(), "{t}");
}

#[tokio::test]
async fn only_the_owner_sees_the_helper_agents() {
    let (r, _) = hub().await;
    let (s, _) = call(&r, "GET", "/api/helpers", RUNNER, json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(get(&r, "/api/helpers").await, json!([]));
}

#[tokio::test]
async fn running_work_shows_its_newest_note_and_its_last_activity() {
    let (r, store) = hub().await;
    let app = app(&r).await;
    let t = ok(
        &r,
        "POST",
        "/api/threads",
        OWNER,
        json!({"app_id": app, "title": "t", "prompt": "p"}),
    )
    .await;
    let id = t["id"].as_i64().expect("id");
    ok(
        &r,
        "POST",
        "/api/runner/claim",
        RUNNER,
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    // It started a while ago; nothing has happened since.
    sqlx::query("UPDATE threads SET updated_at = '2026-01-01T09:00:00.000Z' WHERE id = ?")
        .bind(id)
        .execute(store.pool())
        .await
        .expect("old start");
    let t = get(&r, &format!("/api/threads/{id}")).await;
    assert_eq!(t["last_activity_at"], "2026-01-01T09:00:00.000Z");
    assert!(t.get("last_note").is_none(), "{t}");

    let post = |e: Value| {
        let r = r.clone();
        async move { ok(&r, "POST", &format!("/api/threads/{id}/events"), RUNNER, e).await }
    };
    post(json!({"kind":"message","text":"Build and tests are still running\nin the background; I'll wait for them."})).await;
    post(json!({"kind":"message","text":"   "})).await;
    post(json!({"kind":"tool","name":"Bash","summary":"sleep 540; cat /tmp/out.log"})).await;
    let events = get(&r, &format!("/api/threads/{id}/events")).await;
    let newest = events.as_array().and_then(|e| e.last()).expect("events")["at"].clone();
    let said = events[0]["at"].clone();

    let t = get(&r, &format!("/api/threads/{id}")).await;
    // The newest step, not the start time.
    assert_eq!(t["last_activity_at"], newest, "{t}");
    // The agent's own words, on one line; an empty note is not a note.
    assert_eq!(
        t["last_note"],
        json!({"text": "Build and tests are still running in the background; I'll wait for them.", "at": said})
    );
    let listed = get(&r, "/api/threads").await;
    assert_eq!(listed[0]["last_note"], t["last_note"]);

    // Once it stops running, the note is no longer "what it is doing now".
    ok(
        &r,
        "POST",
        &format!("/api/threads/{id}/move"),
        RUNNER,
        json!({"to":"in_review"}),
    )
    .await;
    let t = get(&r, &format!("/api/threads/{id}")).await;
    assert!(t.get("last_note").is_none(), "{t}");
    assert!(t["last_activity_at"].as_str() >= newest.as_str(), "{t}");
}
