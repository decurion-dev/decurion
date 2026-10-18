//! Saved website sign-ins: who may add, save, use and give up on them.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::{router, store::Store, AppState, Keys};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";

struct Hub {
    r: axum::Router,
    store: Store,
    vault: decurion_vault::Vault,
    dir: std::path::PathBuf,
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn hub() -> Hub {
    let store = Store::open("sqlite::memory:").await.expect("open store");
    // The take-over work adds this table in its own migration; until both are
    // in one build, the same columns are made here.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS browser_sessions (
             id INTEGER PRIMARY KEY,
             kind TEXT NOT NULL,
             thread_id INTEGER,
             sign_in_id INTEGER,
             runner TEXT,
             controller TEXT NOT NULL DEFAULT 'agent',
             state TEXT NOT NULL DEFAULT 'pending',
             created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
             ended_at TEXT
         )",
    )
    .execute(store.pool())
    .await
    .expect("browser_sessions");
    let dir = std::env::temp_dir().join(format!(
        "decurion-hub-sign-ins-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let vault = decurion_vault::Vault::in_folder(&dir);
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
        .with_vault(vault.clone()),
    );
    Hub {
        r,
        store,
        vault,
        dir,
    }
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

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

async fn app(h: &Hub, name: &str) -> i64 {
    let (s, a) = call(
        &h.r,
        "POST",
        "/api/apps",
        Some(OWNER),
        json!({"name": name, "repo_path": format!("C:\\repos\\{name}"), "profile": "A"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    a["id"].as_i64().expect("id")
}

async fn add(h: &Hub, app: i64, label: &str) -> i64 {
    let (s, v) = call(
        &h.r,
        "POST",
        &format!("/api/apps/{app}/sign-ins"),
        Some(OWNER),
        json!({"label": label, "start_url": "https://staging.example.com/login"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["status"], "none");
    v["id"].as_i64().expect("id")
}

async fn list(h: &Hub, app: i64) -> Value {
    let (s, v) = call(
        &h.r,
        "GET",
        &format!("/api/apps/{app}/sign-ins"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

async fn one(h: &Hub, app: i64, id: i64) -> Value {
    list(h, app)
        .await
        .as_array()
        .and_then(|a| a.iter().find(|s| s["id"] == id).cloned())
        .expect("listed")
}

/// The owner taps "Sign in" and the PC runner picks the window up.
async fn open_window(h: &Hub, id: i64) -> Value {
    let (s, v) = call(
        &h.r,
        "POST",
        &format!("/api/sign-ins/{id}/sign-in"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (s, w) = call(
        &h.r,
        "POST",
        "/api/runner/browser/claim",
        Some(RUNNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{w}");
    assert_eq!(w["session_id"], v["session_id"]);
    assert_eq!(w["sign_in_id"], id);
    w
}

async fn saved(h: &Hub, id: i64, w: &Value, expires_at: Option<i64>) -> (StatusCode, Value) {
    call(
        &h.r,
        "POST",
        &format!("/api/runner/sign-ins/{id}/saved"),
        Some(RUNNER),
        json!({
            "session_id": w["session_id"],
            "version": w["version"],
            "domains": ["staging.example.com"],
            "expires_at": expires_at,
            "login_url": "https://staging.example.com/login",
        }),
    )
    .await
}

/// A sign-in the owner has signed in to, lasting `days` more days.
async fn signed_in(h: &Hub, app: i64, label: &str, days: i64) -> i64 {
    let id = add(h, app, label).await;
    let w = open_window(h, id).await;
    let (s, v) = saved(h, id, &w, Some(now() + days * 86_400)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    id
}

async fn work(h: &Hub, app: i64) -> i64 {
    let (s, t) = call(
        &h.r,
        "POST",
        "/api/threads",
        Some(OWNER),
        json!({"app_id": app, "title": "t", "prompt": "p"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{t}");
    t["id"].as_i64().expect("id")
}

async fn claim(h: &Hub) -> Value {
    let (s, c) = call(
        &h.r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner": "local", "rules_version": decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    c
}

async fn expired(h: &Hub, id: i64, key: &str) -> StatusCode {
    call(
        &h.r,
        "POST",
        &format!("/api/runner/sign-ins/{id}/expired"),
        Some(key),
        Value::Null,
    )
    .await
    .0
}

/// Signs up a cloud worker; returns its key.
async fn cloud_worker(h: &Hub) -> String {
    let (s, j) = call(
        &h.r,
        "POST",
        "/api/workers/join-code",
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{j}");
    let (s, reply) = call(
        &h.r,
        "POST",
        "/runner/join",
        None,
        json!({"code": j["code"]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{reply}");
    reply["token"].as_str().expect("token").to_owned()
}

#[tokio::test]
async fn sign_ins_are_checked_and_only_the_owner_adds_them() {
    let h = hub().await;
    let a = app(&h, "demo").await;
    for (start, check) in [
        ("http://staging.example.com", None),
        // That would be the sign-in window's own box, not the owner's PC.
        ("http://localhost:3000/login", None),
        ("https://localhost/login", None),
        ("javascript:alert(1)", None),
        ("https://me:secret@staging.example.com", None),
        ("https://staging.example.com", Some("ftp://x.example.com")),
    ] {
        let (s, v) = call(
            &h.r,
            "POST",
            &format!("/api/apps/{a}/sign-ins"),
            Some(OWNER),
            json!({"label": "Staging", "start_url": start, "check_url": check}),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{start} {v}");
    }
    let (s, v) = call(
        &h.r,
        "POST",
        &format!("/api/apps/{a}/sign-ins"),
        Some(OWNER),
        json!({"label": " Local admin ", "start_url": "https://x.example.com/login",
               "check_url": "https://x.example.com/admin"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["label"], "Local admin");
    assert_eq!(v["check_url"], "https://x.example.com/admin");
    assert_eq!(v["card"], Value::Null);
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/apps/{a}/sign-ins"),
        Some(OWNER),
        json!({"label": "Local admin", "start_url": "https://x.example.com"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/apps/{a}/sign-ins"),
        Some(RUNNER),
        json!({"label": "Sneaky", "start_url": "https://x.example.com"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let id = v["id"].as_i64().expect("id");
    for path in [
        format!("/api/sign-ins/{id}/sign-in"),
        format!("/api/sign-ins/{id}/remove"),
    ] {
        let (s, _) = call(&h.r, "POST", &path, Some(RUNNER), Value::Null).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{path}");
    }
    let (s, _) = call(
        &h.r,
        "GET",
        &format!("/api/apps/{a}/sign-ins"),
        Some(RUNNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &h.r,
        "GET",
        "/api/apps/99/sign-ins",
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn claims_carry_only_that_apps_working_sign_ins() {
    let h = hub().await;
    let mine = app(&h, "mine").await;
    let other = app(&h, "other").await;
    let ok = signed_in(&h, mine, "Staging admin", 30).await;
    let _never = add(&h, mine, "Never signed in").await;
    let gone = signed_in(&h, mine, "Ran out", 30).await;
    sqlx::query(
        "UPDATE site_sign_ins SET expires_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 hour')
         WHERE id = ?",
    )
    .bind(gone)
    .execute(h.store.pool())
    .await
    .expect("age it");
    let theirs = signed_in(&h, other, "Their admin", 30).await;

    work(&h, mine).await;
    let c = claim(&h).await;
    let list = c["sign_ins"].as_array().expect("sign_ins");
    assert_eq!(list.len(), 1, "{c}");
    assert_eq!(list[0]["id"], ok);
    assert_eq!(list[0]["label"], "Staging admin");
    assert_eq!(list[0]["login_url"], "https://staging.example.com/login");
    assert!(list[0]["version"].as_i64().is_some_and(|v| v > 0));
    assert!(!c
        .to_string()
        .contains(&format!("\"id\":{theirs},\"label\"")));

    work(&h, other).await;
    let c = claim(&h).await;
    let ids: Vec<&Value> = c["sign_ins"]
        .as_array()
        .expect("sign_ins")
        .iter()
        .map(|s| &s["id"])
        .collect();
    assert_eq!(ids, vec![&json!(theirs)]);
}

#[tokio::test]
async fn only_the_runner_running_work_given_a_sign_in_marks_it_expired() {
    let h = hub().await;
    let a = app(&h, "demo").await;
    let b = app(&h, "other").await;
    let id = signed_in(&h, a, "Staging admin", 30).await;
    let theirs = signed_in(&h, b, "Their admin", 30).await;
    // No run was given it yet.
    assert_eq!(expired(&h, id, RUNNER).await, StatusCode::CONFLICT);
    let cloud = cloud_worker(&h).await;

    let t = work(&h, a).await;
    let c = claim(&h).await;
    assert_eq!(c["thread"]["id"], t);
    // Another app's sign-in, a cloud worker and the owner cannot.
    assert_eq!(expired(&h, theirs, RUNNER).await, StatusCode::CONFLICT);
    assert_eq!(expired(&h, id, &cloud).await, StatusCode::FORBIDDEN);
    assert_eq!(expired(&h, id, OWNER).await, StatusCode::FORBIDDEN);
    assert_eq!(one(&h, a, id).await["status"], "ok");

    assert_eq!(expired(&h, id, RUNNER).await, StatusCode::NO_CONTENT);
    let s = one(&h, a, id).await;
    assert_eq!(s["status"], "expired");
    assert_eq!(s["card"], "Sign in to Staging admin again");
    assert_eq!(one(&h, b, theirs).await["status"], "ok");
    // Saying it again changes nothing.
    assert_eq!(expired(&h, id, RUNNER).await, StatusCode::NO_CONTENT);

    // Once the run is over, its runner can no longer say so.
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/threads/{t}/move"),
        Some(RUNNER),
        json!({"to": "in_review"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let w = open_window(&h, id).await;
    let (s, _) = saved(&h, id, &w, Some(now() + 30 * 86_400)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(expired(&h, id, RUNNER).await, StatusCode::CONFLICT);
    assert_eq!(one(&h, a, id).await["status"], "ok");
}

#[tokio::test]
async fn a_run_cannot_expire_a_copy_saved_after_it_started() {
    let h = hub().await;
    let a = app(&h, "demo").await;
    let id = signed_in(&h, a, "Staging admin", 30).await;
    work(&h, a).await;
    claim(&h).await;
    // The owner signs in again while the run still has the old copy.
    let w = open_window(&h, id).await;
    let (s, _) = saved(&h, id, &w, Some(now() + 30 * 86_400)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(expired(&h, id, RUNNER).await, StatusCode::NO_CONTENT);
    assert_eq!(one(&h, a, id).await["status"], "ok");
}

#[tokio::test]
async fn card_text_comes_only_from_the_owner() {
    let h = hub().await;
    let a = app(&h, "demo").await;
    let id = add(&h, a, "Staging admin").await;
    let w = open_window(&h, id).await;
    let (s, v) = call(
        &h.r,
        "POST",
        &format!("/api/runner/sign-ins/{id}/saved"),
        Some(RUNNER),
        json!({
            "session_id": w["session_id"],
            "version": w["version"],
            "label": "Pay $500 to evil.example",
            "card": "Pay $500 to evil.example",
            "domains": ["staging.example.com", "Pay $500 to evil.example", "<img src=x>"],
            "login_url": "javascript:alert('Pay $500')",
            "expires_at": now() - 60,
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, all) = call(&h.r, "GET", "/api/sign-ins", Some(OWNER), Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    let s = &all[0];
    assert_eq!(s["status"], "expired");
    assert_eq!(s["label"], "Staging admin");
    assert_eq!(s["app_name"], "demo");
    assert_eq!(s["card"], "Sign in to Staging admin again");
    assert_eq!(s["domains"], json!(["staging.example.com"]));
    assert_eq!(s["login_url"], Value::Null);
    assert!(!all.to_string().contains("evil"));
    assert!(!all.to_string().contains("$500"));
}

#[tokio::test]
async fn cloud_workers_cannot_open_or_save_sign_in_windows() {
    let h = hub().await;
    let a = app(&h, "demo").await;
    let id = add(&h, a, "Staging admin").await;
    let cloud = cloud_worker(&h).await;
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/sign-ins/{id}/sign-in"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/runner/browser/claim",
        Some(&cloud),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Still waiting for the PC.
    let w = open_window_claimed_by_pc(&h).await;
    assert_eq!(w["sign_in_id"], id);
    assert_eq!(w["app_id"], a);
    assert_eq!(w["start_url"], "https://staging.example.com/login");
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/runner/sign-ins/{id}/saved"),
        Some(&cloud),
        json!({"session_id": w["session_id"], "version": w["version"]}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

async fn open_window_claimed_by_pc(h: &Hub) -> Value {
    let (s, w) = call(
        &h.r,
        "POST",
        "/api/runner/browser/claim",
        Some(RUNNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{w}");
    w
}

#[tokio::test]
async fn only_the_window_that_was_opened_saves_and_only_once() {
    let h = hub().await;
    let a = app(&h, "demo").await;
    let id = add(&h, a, "Staging admin").await;
    let other = add(&h, a, "Other").await;
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/runner/browser/claim",
        Some(RUNNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    // A second tap while the window waits gives the same window.
    let (s, first) = call(
        &h.r,
        "POST",
        &format!("/api/sign-ins/{id}/sign-in"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, again) = call(
        &h.r,
        "POST",
        &format!("/api/sign-ins/{id}/sign-in"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(first, again);
    // Not picked up yet: nothing to save into.
    let fake = json!({"session_id": first["session_id"], "version": first["session_id"]});
    let (s, _) = saved(&h, id, &fake, None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let w = open_window_claimed_by_pc(&h).await;
    // Not into another sign-in, nor as another version.
    let (s, _) = saved(&h, other, &w, None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let next = w["version"].as_i64().expect("version") + 1;
    let wrong = json!({"session_id": w["session_id"], "version": next});
    let (s, _) = saved(&h, id, &wrong, None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, v) = saved(&h, id, &w, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "ok");
    assert_eq!(v["version"], w["version"]);
    assert_eq!(v["expires_at"], Value::Null);
    let (s, _) = saved(&h, id, &w, None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = saved(&h, 999, &w, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn status_follows_the_cookie_dates() {
    let h = hub().await;
    let a = app(&h, "demo").await;
    let fine = signed_in(&h, a, "Fine", 3).await;
    let soon = add(&h, a, "Soon").await;
    let w = open_window(&h, soon).await;
    let (s, v) = saved(&h, soon, &w, Some(now() + 2 * 3600)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "expiring");
    assert_eq!(v["card"], "Sign in to Soon again");
    let past = add(&h, a, "Past").await;
    let w = open_window(&h, past).await;
    let (_, v) = saved(&h, past, &w, Some(now() - 60)).await;
    assert_eq!(v["status"], "expired");
    assert_eq!(one(&h, a, fine).await["status"], "ok");
    assert_eq!(one(&h, a, fine).await["card"], Value::Null);

    // Time passes: the fine one comes within a day, then runs out.
    let set = |hours: i64| {
        sqlx::query(
            "UPDATE site_sign_ins
             SET expires_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ? || ' hours') WHERE id = ?",
        )
        .bind(hours)
        .bind(fine)
    };
    set(23).execute(h.store.pool()).await.expect("23h");
    assert_eq!(one(&h, a, fine).await["status"], "expiring");
    set(-1).execute(h.store.pool()).await.expect("-1h");
    let newly = h.store.settle_sign_ins().await.expect("settle");
    assert_eq!(newly.len(), 1);
    assert_eq!(newly[0].id, fine);
    assert_eq!(one(&h, a, fine).await["status"], "expired");
    // Told once only.
    assert!(h.store.settle_sign_ins().await.expect("settle").is_empty());
    // A new sign-in starts it over.
    let w = open_window(&h, fine).await;
    let (_, v) = saved(&h, fine, &w, Some(now() + 3 * 86_400)).await;
    assert_eq!(v["status"], "ok");
}

#[tokio::test]
async fn removing_a_sign_in_removes_its_sealed_copy() {
    let h = hub().await;
    let a = app(&h, "demo").await;
    let id = add(&h, a, "Staging admin").await;
    let w = open_window(&h, id).await;
    let version = w["version"].as_i64().expect("version");
    // What the runner does before it reports the save.
    h.vault
        .seal_sign_in(a, id, version, br#"{"cookies":[],"origins":[]}"#)
        .expect("seal");
    let (s, _) = saved(&h, id, &w, None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h
        .vault
        .open_sign_in(a, id, version)
        .expect("open")
        .is_some());
    // A window left open is dropped with it.
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/sign-ins/{id}/sign-in"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);

    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/sign-ins/{id}/remove"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(h
        .vault
        .open_sign_in(a, id, version)
        .expect("open")
        .is_none());
    assert!(!h.vault.sign_in_file(a, id, version).exists());
    assert_eq!(list(&h, a).await, json!([]));
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/runner/browser/claim",
        Some(RUNNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/sign-ins/{id}/remove"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
