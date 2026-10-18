//! "Run it": who may start, stop and pick up a preview, which work can be
//! run, one preview at a time, and when a preview stops on its own.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::browser::PREVIEW_WAITING;
use decurion_hub::preview::{REPLACED, STOPPED};
use decurion_hub::{router, store::Store, AppState, Keys};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn hub_with(idle: Duration) -> (axum::Router, Store) {
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let r = router(
        AppState::new(
            store.clone(),
            Keys {
                owner: OWNER.into(),
                runner: RUNNER.into(),
            },
            "abc123def456".into(),
            Some("https://hub.test".into()),
        )
        .with_preview_idle(idle),
    );
    (r, store)
}

async fn hub() -> (axum::Router, Store) {
    hub_with(Duration::from_secs(900)).await
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
        .header("authorization", format!("Bearer {key}"));
    let body = if method == "GET" {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let resp = r
        .clone()
        .oneshot(req.body(body).expect("request"))
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

async fn app(r: &axum::Router, name: &str) -> i64 {
    let (s, a) = call(
        r,
        "POST",
        "/api/apps",
        OWNER,
        json!({"name": name, "repo_path": "C:\\repos\\demo", "profile": "A"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    a["id"].as_i64().expect("app id")
}

/// A piece of work of `app`, queued, with nothing saved yet.
async fn piece(r: &axum::Router, app: i64, title: &str) -> i64 {
    let (s, t) = call(
        r,
        "POST",
        "/api/threads",
        OWNER,
        json!({"app_id": app, "title": title, "prompt": "do it", "ai": "claude"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{t}");
    t["id"].as_i64().expect("thread id")
}

/// Gives a piece a branch and a saved copy; returns the copy's id.
async fn saved(store: &Store, thread: i64) -> i64 {
    sqlx::query("UPDATE threads SET branch = ? WHERE id = ?")
        .bind(format!("decurion/thread-{thread}"))
        .bind(thread)
        .execute(store.pool())
        .await
        .expect("branch");
    sqlx::query_scalar(
        "INSERT INTO checkpoints (thread_id, runner, head) VALUES (?, 'runner:local', ?)
         RETURNING id",
    )
    .bind(thread)
    .bind("a".repeat(40))
    .fetch_one(store.pool())
    .await
    .expect("checkpoint")
}

async fn run_app(r: &axum::Router, app: i64) -> Value {
    let (s, p) = call(
        r,
        "POST",
        &format!("/api/apps/{app}/preview"),
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{p}");
    p
}

async fn run_piece(r: &axum::Router, thread: i64) -> (StatusCode, Value) {
    call(
        r,
        "POST",
        &format!("/api/threads/{thread}/preview"),
        OWNER,
        Value::Null,
    )
    .await
}

async fn claim(r: &axum::Router) -> (StatusCode, Value) {
    call(r, "POST", "/api/runner/preview/claim", RUNNER, json!({})).await
}

async fn alive(r: &axum::Router, sid: i64) -> bool {
    let (s, v) = call(
        r,
        "POST",
        &format!("/api/runner/preview/{sid}/alive"),
        RUNNER,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["open"].as_bool().expect("open")
}

async fn state(store: &Store, sid: i64) -> String {
    store.browser_session(sid).await.expect("session").state
}

/// A cloud worker's key.
async fn cloud_key(store: &Store) -> String {
    let (code, _) = store.new_join_code().await.expect("join code");
    let (_, token) = store
        .join_cloud_worker(&code)
        .await
        .expect("join")
        .expect("worker");
    token
}

#[tokio::test]
async fn only_the_owner_starts_stops_and_lists_previews() {
    let (r, store) = hub().await;
    let a = app(&r, "shop").await;
    let t = piece(&r, a, "New price list").await;
    saved(&store, t).await;
    for path in [
        format!("/api/threads/{t}/preview"),
        format!("/api/apps/{a}/preview"),
    ] {
        let (s, _) = call(&r, "POST", &path, RUNNER, Value::Null).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{path}");
    }
    let sid = run_app(&r, a).await["id"].as_i64().expect("id");
    let (s, _) = call(&r, "GET", "/api/previews", RUNNER, Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/browser/{sid}/stop"),
        RUNNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // And the runner's end is the runner's alone.
    let (s, _) = call(&r, "POST", "/api/runner/preview/claim", OWNER, json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/runner/preview/{sid}/alive"),
        OWNER,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(state(&store, sid).await, "pending");
}

#[tokio::test]
async fn work_with_nothing_new_to_run_is_refused_in_plain_words() {
    let (r, store) = hub().await;
    let a = app(&r, "shop").await;

    // Nothing saved yet.
    let t = piece(&r, a, "New price list").await;
    let (s, v) = run_piece(&r, t).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(
        v,
        "Nothing has been saved for this work yet. Run it once the agent has finished a run."
    );

    // A saved copy but no branch of its own.
    sqlx::query("INSERT INTO checkpoints (thread_id, runner, head) VALUES (?, 'runner:local', ?)")
        .bind(t)
        .bind("b".repeat(40))
        .execute(store.pool())
        .await
        .expect("checkpoint");
    let (s, v) = run_piece(&r, t).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");

    // Research changes no code.
    let research = piece(&r, a, "How sign-in works").await;
    saved(&store, research).await;
    sqlx::query("UPDATE threads SET research = 1 WHERE id = ?")
        .bind(research)
        .execute(store.pool())
        .await
        .expect("research");
    let (s, v) = run_piece(&r, research).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(
        v,
        "Research changes no code, so there is nothing new to run."
    );

    // Nothing there at all.
    let (s, _) = run_piece(&r, 999).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(&r, "POST", "/api/apps/999/preview", OWNER, Value::Null).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (s, v) = call(&r, "GET", "/api/previews", OWNER, Value::Null).await;
    assert_eq!((s, v), (StatusCode::OK, json!([])));
}

#[tokio::test]
async fn the_pc_runner_picks_up_an_app_preview_with_its_test_key_names() {
    let (r, store) = hub().await;
    let a = app(&r, "shop").await;
    store
        .save_test_key(a, "STRIPE_TEST_KEY", "4242")
        .await
        .expect("test key");
    let (s, _) = claim(&r).await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    let p = run_app(&r, a).await;
    assert_eq!(p["kind"], "preview");
    assert_eq!(p["app_id"], a);
    assert_eq!(p["thread_id"], Value::Null);
    assert_eq!(p["controller"], "owner");
    assert_eq!(p["state"], "pending");
    let sid = p["id"].as_i64().expect("id");

    let (s, c) = claim(&r).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["session_id"], sid);
    assert_eq!(c["app"]["id"], a);
    assert_eq!(c["app"]["name"], "shop");
    assert_eq!(c["thread"], Value::Null);
    assert_eq!(c["test_keys"], json!(["STRIPE_TEST_KEY"]));
    // Names only: the hub never has the value.
    assert!(!c.to_string().contains("4242"), "{c}");
    let s = store.browser_session(sid).await.expect("session");
    assert_eq!(s.runner.as_deref(), Some("local"));
    assert!(s.is_open());

    // Taken once.
    let (s, _) = claim(&r).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(alive(&r, sid).await);
    let (s, v) = call(&r, "GET", "/api/previews", OWNER, Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v.as_array().map(Vec::len), Some(1), "{v}");
    assert_eq!(v[0]["id"], sid);
}

#[tokio::test]
async fn a_piece_preview_runs_its_branch_and_latest_saved_copy() {
    let (r, store) = hub().await;
    let a = app(&r, "shop").await;
    let t = piece(&r, a, "New price list").await;
    saved(&store, t).await;
    let latest = saved(&store, t).await;
    let (s, p) = run_piece(&r, t).await;
    assert_eq!(s, StatusCode::OK, "{p}");
    assert_eq!(p["thread_id"], t);
    assert_eq!(p["app_id"], a);

    let (s, c) = claim(&r).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(
        c["thread"],
        json!({
            "thread_id": t,
            "title": "New price list",
            "branch": format!("decurion/thread-{t}"),
            "checkpoint": latest,
        })
    );
}

#[tokio::test]
async fn a_piece_that_lost_its_saved_copy_is_not_picked_up() {
    let (r, store) = hub().await;
    let a = app(&r, "shop").await;
    let t = piece(&r, a, "New price list").await;
    saved(&store, t).await;
    let (_, p) = run_piece(&r, t).await;
    let sid = p["id"].as_i64().expect("id");
    sqlx::query("DELETE FROM checkpoints WHERE thread_id = ?")
        .bind(t)
        .execute(store.pool())
        .await
        .expect("forget copies");
    let (s, _) = claim(&r).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(state(&store, sid).await, "failed");
}

#[tokio::test]
async fn a_second_tap_reuses_and_another_target_replaces() {
    let (r, store) = hub().await;
    let a = app(&r, "shop").await;
    let t = piece(&r, a, "New price list").await;
    saved(&store, t).await;

    let first = run_app(&r, a).await["id"].as_i64().expect("id");
    assert_eq!(run_app(&r, a).await["id"], first);
    let (s, _) = claim(&r).await;
    assert_eq!(s, StatusCode::OK);
    // Still the same once the PC has it.
    assert_eq!(run_app(&r, a).await["id"], first);
    assert!(alive(&r, first).await);

    // The app's piece of work is another target: the first one stops.
    let (s, p) = run_piece(&r, t).await;
    assert_eq!(s, StatusCode::OK, "{p}");
    let second = p["id"].as_i64().expect("id");
    assert_ne!(second, first);
    assert_eq!(state(&store, first).await, "closed");
    assert!(!alive(&r, first).await);
    let (_, v) = call(&r, "GET", "/api/previews", OWNER, Value::Null).await;
    assert_eq!(v.as_array().map(Vec::len), Some(1), "{v}");
    assert_eq!(v[0]["id"], second);
    let (s, again) = run_piece(&r, t).await;
    assert_eq!((s, &again["id"]), (StatusCode::OK, &json!(second)));

    // And the database itself keeps it to one.
    let two = sqlx::query(
        "INSERT INTO browser_sessions (kind, app_id, controller) VALUES ('preview', ?, 'owner')",
    )
    .bind(a)
    .execute(store.pool())
    .await;
    assert!(two.is_err(), "a second open preview was allowed");
    let no_app =
        sqlx::query("INSERT INTO browser_sessions (kind, state) VALUES ('preview', 'closed')")
            .execute(store.pool())
            .await;
    assert!(no_app.is_err(), "a preview without an app was allowed");
}

#[tokio::test]
async fn the_owner_stops_a_preview() {
    let (r, store) = hub().await;
    let a = app(&r, "shop").await;
    let sid = run_app(&r, a).await["id"].as_i64().expect("id");
    claim(&r).await;
    assert!(alive(&r, sid).await);
    let stop = format!("/api/browser/{sid}/stop");
    let (s, v) = call(&r, "POST", &stop, OWNER, Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "closed");
    assert!(!alive(&r, sid).await);
    // Again is fine, and changes nothing.
    let (s, again) = call(&r, "POST", &stop, OWNER, Value::Null).await;
    assert_eq!((s, &again), (StatusCode::OK, &v));
    let (s, _) = call(&r, "POST", "/api/browser/999/stop", OWNER, Value::Null).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Only a preview is stopped here.
    let t = piece(&r, a, "New price list").await;
    let other: i64 = sqlx::query_scalar(
        "INSERT INTO browser_sessions (kind, thread_id) VALUES ('take_over', ?) RETURNING id",
    )
    .bind(t)
    .fetch_one(store.pool())
    .await
    .expect("take-over view");
    let (s, v) = call(
        &r,
        "POST",
        &format!("/api/browser/{other}/stop"),
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v, "Only a preview can be stopped here.");
    assert_eq!(state(&store, other).await, "pending");
}

#[tokio::test]
async fn the_runner_ends_its_own_preview() {
    let (r, store) = hub().await;
    let a = app(&r, "shop").await;
    let sid = run_app(&r, a).await["id"].as_i64().expect("id");
    let end = format!("/api/runner/preview/{sid}/end");
    let body = json!({"why": "The app did not start.", "failed": true});

    // Not picked up by this runner yet.
    let (s, _) = call(&r, "POST", &end, RUNNER, body.clone()).await;
    assert_eq!(s, StatusCode::CONFLICT);
    claim(&r).await;
    let (s, _) = call(&r, "POST", &end, OWNER, body.clone()).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let cloud = cloud_key(&store).await;
    let (s, _) = call(&r, "POST", &end, &cloud, body.clone()).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let (s, v) = call(&r, "POST", &end, RUNNER, body.clone()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "failed");
    // Once over, saying so again changes nothing.
    let (s, again) = call(
        &r,
        "POST",
        &end,
        RUNNER,
        json!({"why": "Stopped.", "failed": false}),
    )
    .await;
    assert_eq!((s, &again["state"]), (StatusCode::OK, &json!("failed")));
    assert!(!alive(&r, sid).await);

    // Not a preview.
    let t = piece(&r, a, "New price list").await;
    let other: i64 = sqlx::query_scalar(
        "INSERT INTO browser_sessions (kind, thread_id, runner) VALUES ('take_over', ?, 'local')
         RETURNING id",
    )
    .bind(t)
    .fetch_one(store.pool())
    .await
    .expect("take-over view");
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/runner/preview/{other}/end"),
        RUNNER,
        body.clone(),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(!alive(&r, other).await);
    assert_eq!(state(&store, other).await, "pending");
}

#[tokio::test]
async fn cloud_workers_cannot_pick_up_previews() {
    let (r, store) = hub().await;
    let a = app(&r, "shop").await;
    let sid = run_app(&r, a).await["id"].as_i64().expect("id");
    let cloud = cloud_key(&store).await;
    let (s, _) = call(&r, "POST", "/api/runner/preview/claim", &cloud, json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/runner/preview/{sid}/alive"),
        &cloud,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    for path in [
        format!("/api/apps/{a}/preview"),
        "/api/previews".to_owned(),
        format!("/api/browser/{sid}/stop"),
    ] {
        let method = if path == "/api/previews" {
            "GET"
        } else {
            "POST"
        };
        let (s, _) = call(&r, method, &path, &cloud, Value::Null).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{path}");
    }
    let s = store.browser_session(sid).await.expect("session");
    assert_eq!((s.state.as_str(), s.runner), ("pending", None));
}

#[tokio::test]
async fn a_preview_nobody_has_open_stops_on_its_own() {
    let (r, store) = hub_with(Duration::from_millis(300)).await;
    let a = app(&r, "shop").await;
    let sid = run_app(&r, a).await["id"].as_i64().expect("id");
    claim(&r).await;
    // The clock starts when the hub first hears of it.
    assert!(alive(&r, sid).await);

    // While the owner's screen is on it, it keeps running.
    let addr = serve(r.clone()).await;
    let mut viewer = connect(addr, &format!("/api/browser/{sid}/ws"), OWNER).await;
    assert_eq!(
        next(&mut viewer).await,
        json!({"type": "control", "who": "owner"})
    );
    assert_eq!(
        next(&mut viewer).await,
        json!({"type": "ended", "why": PREVIEW_WAITING})
    );
    tokio::time::sleep(Duration::from_millis(450)).await;
    assert!(alive(&r, sid).await);

    // Once it leaves, the clock runs again.
    drop(viewer);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(alive(&r, sid).await);
    tokio::time::sleep(Duration::from_millis(450)).await;
    assert!(!alive(&r, sid).await);
    assert_eq!(state(&store, sid).await, "closed");
}

#[tokio::test]
async fn screens_are_told_why_a_preview_ended() {
    let (r, store) = hub().await;
    let a = app(&r, "shop").await;
    let t = piece(&r, a, "New price list").await;
    saved(&store, t).await;
    let addr = serve(r.clone()).await;

    // Replaced by another.
    let first = run_app(&r, a).await["id"].as_i64().expect("id");
    let mut viewer = connect(addr, &format!("/api/browser/{first}/ws"), OWNER).await;
    next(&mut viewer).await;
    next(&mut viewer).await;
    let (_, p) = run_piece(&r, t).await;
    let second = p["id"].as_i64().expect("id");
    assert_eq!(
        next(&mut viewer).await,
        json!({"type": "ended", "why": REPLACED})
    );
    assert!(closed(&mut viewer).await);

    // Stopped by the owner.
    let mut viewer = connect(addr, &format!("/api/browser/{second}/ws"), OWNER).await;
    next(&mut viewer).await;
    next(&mut viewer).await;
    call(
        &r,
        "POST",
        &format!("/api/browser/{second}/stop"),
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(
        next(&mut viewer).await,
        json!({"type": "ended", "why": STOPPED})
    );
    assert!(closed(&mut viewer).await);

    // Ended by the runner, its reason cut short.
    let third = run_app(&r, a).await["id"].as_i64().expect("id");
    claim(&r).await;
    let mut viewer = connect(addr, &format!("/api/browser/{third}/ws"), OWNER).await;
    next(&mut viewer).await;
    next(&mut viewer).await;
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/runner/preview/{third}/end"),
        RUNNER,
        json!({"why": "x".repeat(5000), "failed": true}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        next(&mut viewer).await,
        json!({"type": "ended", "why": "x".repeat(4000), "failed": true})
    );
    assert!(closed(&mut viewer).await);

    // Failed before any screen was open: one that opens later is still
    // told why, and that an agent could fix it.
    let fourth = run_app(&r, a).await["id"].as_i64().expect("id");
    claim(&r).await;
    let (s, v) = call(
        &r,
        "POST",
        &format!("/api/runner/preview/{fourth}/end"),
        RUNNER,
        json!({"why": "No recipe.", "failed": true, "fixable": true}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (&v["why"], &v["fixable"]),
        (&json!("No recipe."), &json!(true))
    );
    let mut late = connect(addr, &format!("/api/browser/{fourth}/ws"), OWNER).await;
    assert_eq!(
        next(&mut late).await,
        json!({"type": "ended", "why": "No recipe.", "failed": true, "fixable": true})
    );
    assert!(closed(&mut late).await);

    // One stopped with no reason kept is told it has ended.
    let mut late = connect(addr, &format!("/api/browser/{second}/ws"), OWNER).await;
    assert_eq!(
        next(&mut late).await,
        json!({"type": "ended", "why": "This live view has ended."})
    );
    assert!(closed(&mut late).await);
}

#[tokio::test]
async fn the_runner_connects_to_a_preview_it_picked_up() {
    let (r, _) = hub().await;
    let a = app(&r, "shop").await;
    let sid = run_app(&r, a).await["id"].as_i64().expect("id");
    let addr = serve(r.clone()).await;
    // Not before it picks it up.
    let path = format!("/api/runner/browser/{sid}/ws");
    let (s, _) = call(&r, "GET", &path, RUNNER, Value::Null).await;
    assert_eq!(s, StatusCode::CONFLICT);
    claim(&r).await;
    let mut runner = connect(addr, &path, RUNNER).await;
    assert_eq!(
        next(&mut runner).await,
        json!({"type": "control", "who": "owner"})
    );
    let (_, v) = call(&r, "GET", "/api/previews", OWNER, Value::Null).await;
    assert_eq!(v[0]["state"], "live", "{v}");
}

#[tokio::test]
async fn previews_whose_pc_went_quiet_are_closed() {
    let (r, store) = hub().await;
    let a = app(&r, "shop").await;

    // Picked up, then never heard from again.
    let quiet = run_app(&r, a).await["id"].as_i64().expect("id");
    claim(&r).await;
    sqlx::query("UPDATE browser_sessions SET seen_at = '2020-01-01T00:00:00.000Z' WHERE id = ?")
        .bind(quiet)
        .execute(store.pool())
        .await
        .expect("age it");
    let (_, v) = call(&r, "GET", "/api/previews", OWNER, Value::Null).await;
    assert_eq!(v, json!([]));
    assert_eq!(state(&store, quiet).await, "failed");
    assert!(!alive(&r, quiet).await);

    // Never picked up.
    let waiting = run_app(&r, a).await["id"].as_i64().expect("id");
    sqlx::query("UPDATE browser_sessions SET created_at = '2020-01-01T00:00:00.000Z' WHERE id = ?")
        .bind(waiting)
        .execute(store.pool())
        .await
        .expect("age it");
    let (s, _) = claim(&r).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(state(&store, waiting).await, "closed");
    // A new tap starts afresh.
    assert_ne!(run_app(&r, a).await["id"], waiting);
}

/// One browser_sessions row: id, kind, thread, sign-in, controller, state,
/// when it ended.
type Row = (
    i64,
    String,
    Option<i64>,
    Option<i64>,
    String,
    String,
    Option<String>,
);

/// The rebuild of browser_sessions keeps the rows already there.
#[tokio::test]
async fn older_browser_sessions_survive_the_rebuild() {
    use sqlx::migrate::Migrator;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    let all = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let before = std::env::temp_dir().join(format!(
        "decurion-preview-migrations-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&before).expect("temp dir");
    for f in std::fs::read_dir(&all).expect("migrations") {
        let f = f.expect("entry").path();
        let name = f.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name.as_bytes().first().is_some_and(u8::is_ascii_digit) && name < "0085" {
            std::fs::copy(&f, before.join(name)).expect("copy");
        }
    }
    let opts = SqliteConnectOptions::from_str("sqlite::memory:")
        .expect("options")
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("pool");
    Migrator::new(before.as_path())
        .await
        .expect("older migrations")
        .run(&pool)
        .await
        .expect("run older");
    let _ = std::fs::remove_dir_all(&before);

    sqlx::query(
        "INSERT INTO apps (name, repo_path, profile) VALUES ('shop', 'C:/apps/shop', 'A');
         INSERT INTO threads (app_id, title, prompt) VALUES (1, 't', 'p');
         INSERT INTO browser_sessions (id, kind, thread_id, runner, controller, state)
             VALUES (7, 'take_over', 1, 'local', 'owner', 'live');
         INSERT INTO browser_sessions (id, kind, sign_in_id, controller, state, ended_at)
             VALUES (9, 'sign_in', 3, 'owner', 'saved', '2026-01-01T00:00:00.000Z');",
    )
    .execute(&pool)
    .await
    .expect("older rows");

    Migrator::new(all.as_path())
        .await
        .expect("migrations")
        .run(&pool)
        .await
        .expect("run all");
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, kind, thread_id, sign_in_id, controller, state, ended_at
             FROM browser_sessions ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("rows");
    assert_eq!(
        rows,
        vec![
            (
                7,
                "take_over".into(),
                Some(1),
                None,
                "owner".into(),
                "live".into(),
                None
            ),
            (
                9,
                "sign_in".into(),
                None,
                Some(3),
                "owner".into(),
                "saved".into(),
                Some("2026-01-01T00:00:00.000Z".into())
            ),
        ]
    );
    // The take-over rule still holds after the rebuild.
    let twice = sqlx::query(
        "INSERT INTO browser_sessions (kind, thread_id, state) VALUES ('take_over', 1, 'pending')",
    )
    .execute(&pool)
    .await;
    assert!(twice.is_err(), "two open take-over views of one run");
}

// ---------- Sockets ----------

async fn serve(r: axum::Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, r).await });
    addr
}

async fn connect(addr: std::net::SocketAddr, path: &str, key: &str) -> Ws {
    let mut req = format!("ws://{addr}{path}")
        .into_client_request()
        .expect("request");
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {key}").parse().expect("header"),
    );
    tokio_tungstenite::connect_async(req)
        .await
        .expect("connect")
        .0
}

/// The next JSON message, skipping pings.
async fn next(ws: &mut Ws) -> Value {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("a message in time")
            .expect("still open")
            .expect("no error");
        if let Message::Text(t) = m {
            return serde_json::from_str(t.as_str()).expect("json");
        }
    }
}

/// True once the hub has closed the socket.
async fn closed(ws: &mut Ws) -> bool {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
            Err(_) => return false,
            Ok(None | Some(Err(_)) | Some(Ok(Message::Close(_)))) => return true,
            Ok(Some(Ok(_))) => continue,
        }
    }
}
