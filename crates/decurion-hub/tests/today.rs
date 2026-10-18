//! The Today strip (`GET /api/today`): one row per app saying what it waits on.

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

async fn add_app(r: &axum::Router, name: &str) -> i64 {
    let (s, app) = call(
        r,
        "POST",
        "/api/apps",
        Some(OWNER),
        json!({"name": name, "repo_path": format!("C:\\repos\\{name}"), "profile": "A"}),
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
    t["id"].as_i64().expect("thread id")
}

/// Puts work straight into a state, as the runner and the owner would.
async fn set_state(store: &Store, thread: i64, state: &str) {
    sqlx::query("UPDATE threads SET state = ? WHERE id = ?")
        .bind(state)
        .bind(thread)
        .execute(store.pool())
        .await
        .expect("set state");
}

/// A pull request Decurion watches, as the GitHub watcher records it.
async fn watch(store: &Store, thread: i64, app: i64, number: i64, state: &str, event_at: &str) {
    sqlx::query(
        "INSERT INTO pr_watches (thread_id, app_id, owner, repo, number, url, branch, head, state, event, event_at)
         VALUES (?, ?, 'me', 'app', ?, ?, ?, 'abc123', ?, ?, ?)",
    )
    .bind(thread)
    .bind(app)
    .bind(number)
    .bind(format!("https://github.com/me/app/pull/{number}"))
    .bind(format!("decurion/app-{thread}"))
    .bind(state)
    .bind(format!("It is {state}"))
    .bind(event_at)
    .execute(store.pool())
    .await
    .expect("watch");
}

async fn comment(store: &Store, thread: i64, gh_id: i64, author: &str) {
    sqlx::query(
        "INSERT INTO pr_comments (thread_id, kind, gh_id, author, body, at)
         VALUES (?, 'line', ?, ?, 'Please rename this.', '2026-10-18T09:00:00Z')",
    )
    .bind(thread)
    .bind(gh_id)
    .bind(author)
    .execute(store.pool())
    .await
    .expect("comment");
}

async fn today(r: &axum::Router) -> Vec<Value> {
    let (s, v) = call(r, "GET", "/api/today", Some(OWNER), Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v.as_array().expect("a list").clone()
}

#[tokio::test]
async fn an_empty_hub_has_nothing_today() {
    let (r, _) = hub().await;
    assert!(today(&r).await.is_empty());
}

#[tokio::test]
async fn an_app_with_nothing_still_shows_quietly() {
    let (r, _) = hub().await;
    let app = add_app(&r, "family-budget").await;
    let rows = today(&r).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["app_id"], app);
    assert_eq!(rows[0]["name"], "family-budget");
    assert_eq!(rows[0]["tone"], "quiet");
    assert_eq!(rows[0]["line"], "No work yet");
    assert_eq!(rows[0]["last_pr"], Value::Null);
    assert_eq!(rows[0]["first_today"], false);
}

#[tokio::test]
async fn an_app_with_running_work_says_working() {
    let (r, store) = hub().await;
    let app = add_app(&r, "atlas").await;
    let t = add_work(&r, app, "Build the map").await;
    set_state(&store, t, "running").await;
    add_work(&r, app, "Then the legend").await;
    let rows = today(&r).await;
    assert_eq!(rows[0]["tone"], "working");
    assert_eq!(rows[0]["status"], "Working");
    assert_eq!(rows[0]["running"], 1);
    assert_eq!(rows[0]["queued"], 1);
    assert_eq!(rows[0]["line"], "1 running, 1 waiting to start");
}

#[tokio::test]
async fn work_that_needs_you_puts_its_app_first() {
    let (r, store) = hub().await;
    let a = add_app(&r, "atlas").await;
    let t = add_work(&r, a, "Build the map").await;
    set_state(&store, t, "running").await;
    add_app(&r, "be-safe-emr").await;
    let z = add_app(&r, "zebra-notes").await;
    let t = add_work(&r, z, "Ask about colours").await;
    set_state(&store, t, "needs_you").await;

    let rows = today(&r).await;
    let names: Vec<&str> = rows
        .iter()
        .map(|x| x["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, ["zebra-notes", "atlas", "be-safe-emr"]);
    assert_eq!(rows[0]["tone"], "needs_you");
    assert_eq!(rows[0]["status"], "Needs you");
    assert_eq!(rows[0]["needs_you"], 1);
    assert_eq!(rows[0]["line"], "1 piece needs you");
    assert_eq!(rows[2]["tone"], "quiet");
}

#[tokio::test]
async fn a_ready_plan_needs_you_and_a_working_goal_counts_its_pieces() {
    let (r, store) = hub().await;
    let refill = add_app(&r, "refill-tracker").await;
    sqlx::query("INSERT INTO goals (app_id, title, state, session_id) VALUES (?, 'Refills', 'plan_ready', 's1')")
        .bind(refill)
        .execute(store.pool())
        .await
        .expect("goal");
    let atlas = add_app(&r, "atlas").await;
    let goal: i64 = sqlx::query_scalar(
        "INSERT INTO goals (app_id, title, state, session_id) VALUES (?, 'Map', 'working', 's2') RETURNING id",
    )
    .bind(atlas)
    .fetch_one(store.pool())
    .await
    .expect("goal");
    for (i, review) in ["passed", "passed", "building", "building"]
        .iter()
        .enumerate()
    {
        let t = add_work(&r, atlas, &format!("Piece {i}")).await;
        set_state(
            &store,
            t,
            if *review == "passed" {
                "in_review"
            } else {
                "running"
            },
        )
        .await;
        sqlx::query("INSERT INTO goal_pieces (thread_id, goal_id, title, done_when, review) VALUES (?, ?, ?, 'it works', ?)")
            .bind(t)
            .bind(goal)
            .bind(format!("Piece {i}"))
            .bind(review)
            .execute(store.pool())
            .await
            .expect("piece");
    }

    let rows = today(&r).await;
    let refill_row = rows.iter().find(|x| x["app_id"] == refill).expect("refill");
    assert_eq!(refill_row["tone"], "needs_you");
    assert_eq!(refill_row["line"], "Plan ready");
    assert_eq!(refill_row["needs_you"], 1);
    let atlas_row = rows.iter().find(|x| x["app_id"] == atlas).expect("atlas");
    assert_eq!(
        atlas_row["pieces"],
        json!({"goal_id": goal, "done": 2, "total": 4})
    );
    // Two pieces wait for the owner to check them, so Atlas needs them too.
    assert_eq!(atlas_row["tone"], "needs_you");
    assert_eq!(atlas_row["line"], "2 pieces are ready to check");
    assert_eq!(atlas_row["running"], 2);
}

#[tokio::test]
async fn a_working_goal_with_nothing_to_check_shows_its_progress() {
    let (r, store) = hub().await;
    let atlas = add_app(&r, "atlas").await;
    let goal: i64 = sqlx::query_scalar(
        "INSERT INTO goals (app_id, title, state, session_id) VALUES (?, 'Map', 'working', 's2') RETURNING id",
    )
    .bind(atlas)
    .fetch_one(store.pool())
    .await
    .expect("goal");
    for (i, (state, review)) in [
        ("done", "passed"),
        ("running", "building"),
        ("queued", "building"),
    ]
    .iter()
    .enumerate()
    {
        let t = add_work(&r, atlas, &format!("Piece {i}")).await;
        set_state(&store, t, state).await;
        sqlx::query("INSERT INTO goal_pieces (thread_id, goal_id, title, done_when, review) VALUES (?, ?, 'p', 'it works', ?)")
            .bind(t)
            .bind(goal)
            .bind(review)
            .execute(store.pool())
            .await
            .expect("piece");
    }
    let rows = today(&r).await;
    assert_eq!(rows[0]["tone"], "working");
    assert_eq!(rows[0]["line"], "1 of 3 pieces built");
}

#[tokio::test]
async fn review_comments_on_an_open_pull_request_ask_for_a_fix() {
    let (r, store) = hub().await;
    let app = add_app(&r, "be-safe-emr").await;
    sqlx::query("UPDATE apps SET reviewer = 'param' WHERE id = ?")
        .bind(app)
        .execute(store.pool())
        .await
        .expect("reviewer");
    let t = add_work(&r, app, "Add the allergy list").await;
    set_state(&store, t, "done").await;
    watch(&store, t, app, 88, "open", "2026-10-18T08:00:00Z").await;
    for id in 1..=3 {
        comment(&store, t, id, "param").await;
    }
    // A comment a fix was already started for is not counted again.
    comment(&store, t, 4, "param").await;
    let fix = add_work(&r, app, "Fix the comments").await;
    sqlx::query("UPDATE pr_comments SET fix_thread = ? WHERE gh_id = 4")
        .bind(fix)
        .execute(store.pool())
        .await
        .expect("fixed");
    set_state(&store, fix, "done").await;

    let rows = today(&r).await;
    let row = &rows[0];
    assert_eq!(row["tone"], "needs_you");
    assert_eq!(row["line"], "PR #88 · param left 3 comments");
    assert!(row["line"]
        .as_str()
        .expect("line")
        .contains("left 3 comments"));
    assert_eq!(row["waiting_on_reviewer"], 1);
    assert_eq!(row["open_comments"], 3);
    assert_eq!(row["fix_thread_id"], t);
    assert_eq!(row["reviewer"], "param");
    assert_eq!(
        row["last_pr"],
        json!({
            "thread_id": t, "number": 88, "url": "https://github.com/me/app/pull/88",
            "state": "open", "event": "It is open", "event_at": "2026-10-18T08:00:00Z",
        })
    );

    // Once the fix is started for all of them, the pull request waits on the reviewer.
    sqlx::query("UPDATE pr_comments SET fix_thread = ?")
        .bind(fix)
        .execute(store.pool())
        .await
        .expect("all fixed");
    let rows = today(&r).await;
    assert_eq!(rows[0]["tone"], "reviewer");
    assert_eq!(rows[0]["line"], "PR #88 · waiting on param");
    assert_eq!(rows[0]["open_comments"], 0);
    assert_eq!(rows[0]["fix_thread_id"], Value::Null);
}

#[tokio::test]
async fn a_merged_pull_request_is_the_last_word() {
    let (r, store) = hub().await;
    let app = add_app(&r, "family-budget").await;
    let old = add_work(&r, app, "First change").await;
    let new = add_work(&r, app, "Second change").await;
    set_state(&store, old, "done").await;
    set_state(&store, new, "done").await;
    watch(&store, new, app, 12, "merged", "2026-10-18T07:30:00Z").await;
    watch(&store, old, app, 11, "closed", "2026-10-17T07:30:00Z").await;

    let rows = today(&r).await;
    assert_eq!(rows[0]["tone"], "done");
    assert_eq!(rows[0]["status"], "Done");
    assert!(rows[0]["line"].as_str().expect("line").contains("merged"));
    assert_eq!(rows[0]["line"], "PR #12 merged");
    assert_eq!(rows[0]["last_pr"]["number"], 12);
    assert_eq!(rows[0]["waiting_on_reviewer"], 0);
}

#[tokio::test]
async fn dismissing_the_merged_work_quiets_its_row() {
    let (r, store) = hub().await;
    let app = add_app(&r, "family-budget").await;
    let old = add_work(&r, app, "First change").await;
    let new = add_work(&r, app, "Second change").await;
    set_state(&store, old, "done").await;
    set_state(&store, new, "done").await;
    watch(&store, new, app, 12, "merged", "2026-10-18T07:30:00Z").await;
    watch(&store, old, app, 11, "merged", "2026-10-17T07:30:00Z").await;

    let (s, v) = call(
        &r,
        "POST",
        "/api/threads/dismiss",
        Some(OWNER),
        json!({"ids": [new]}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{v}");
    let rows = today(&r).await;
    // Old news is not repeated, and the older pull request does not take
    // its place.
    assert_eq!(rows[0]["tone"], "quiet");
    assert_eq!(rows[0]["line"], "Nothing waiting");
    assert!(rows[0]["last_pr"].is_null());
}

#[tokio::test]
async fn only_the_owner_sees_today() {
    let (r, _) = hub().await;
    add_app(&r, "atlas").await;
    let (s, v) = call(&r, "GET", "/api/today", Some(RUNNER), Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    let (s, _) = call(&r, "GET", "/api/today", None, Value::Null).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_board_and_inbox_still_send_the_content_security_policy() {
    let (r, _) = hub().await;
    for path in ["/", "/inbox"] {
        let resp = r
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).expect("req"))
            .await
            .expect("resp");
        let csp = resp
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(csp.contains("frame-ancestors 'none'"), "{path}");
    }
}
