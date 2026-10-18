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

async fn claim(r: &axum::Router) -> Value {
    let (s, c) = call(
        r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    c
}

async fn seed(r: &axum::Router) -> i64 {
    let (s, app) = call(
        r,
        "POST",
        "/api/apps",
        Some(OWNER),
        json!({"name":"demo","repo_path":"C:\\repos\\demo","profile":"A"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, t) = call(
        r,
        "POST",
        "/api/threads",
        Some(OWNER),
        json!({"app_id": app["id"], "title":"t1", "prompt":"do it"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(t["state"], "queued");
    t["id"].as_i64().expect("id")
}

#[tokio::test]
async fn api_requires_a_key() {
    let (r, _) = hub().await;
    let (s, _) = call(&r, "GET", "/api/threads", None, Value::Null).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = call(&r, "GET", "/api/threads", Some("guess"), Value::Null).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = call(&r, "GET", "/api/threads", Some(OWNER), Value::Null).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn runner_cannot_add_work_or_mark_done() {
    let (r, _) = hub().await;
    let id = seed(&r).await;
    let (s, _) = call(
        &r,
        "POST",
        "/api/threads",
        Some(RUNNER),
        json!({"app_id":1,"title":"x","prompt":"y"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, c) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(c["thread"]["id"], id);
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(RUNNER),
        json!({"to":"done"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, t) = call(
        &r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(RUNNER),
        json!({"to":"in_review"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t["state"], "in_review");
    // Once out of Running, the runner cannot touch it.
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(RUNNER),
        json!({"to":"failed"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, t) = call(
        &r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(OWNER),
        json!({"to":"done"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t["state"], "done");
}

#[tokio::test]
async fn owner_cannot_make_illegal_moves() {
    let (r, _) = hub().await;
    let id = seed(&r).await;
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(OWNER),
        json!({"to":"done"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(OWNER),
        json!({"to":"paused"}),
    )
    .await;
    assert!(s.is_client_error());
}

#[tokio::test]
async fn claim_hands_each_thread_out_once() {
    let (r, _) = hub().await;
    seed(&r).await;
    let (s, first) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(first["resume"], false);
    assert!(first["thread"]["session_id"]
        .as_str()
        .is_some_and(|s| s.len() == 36));
    let (s, _) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn runner_restart_resumes_the_same_session() {
    let (r, store) = hub().await;
    let id = seed(&r).await;
    let (_, first) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    let session = first["thread"]["session_id"].clone();
    // The agent got as far as saying something.
    call(
        &r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(RUNNER),
        json!({"kind":"message","text":"working"}),
    )
    .await;

    // Runner dies and comes back.
    let (s, paused) = call(
        &r,
        "POST",
        "/api/runner/hello",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(paused["paused"], json!([id]));
    assert_eq!(
        store.thread(id).await.expect("thread").state,
        decurion_proto::ThreadState::Queued
    );

    let (_, again) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(again["resume"], true);
    assert_eq!(again["thread"]["session_id"], session);
    assert_eq!(again["thread"]["branch"], first["thread"]["branch"]);
    // started, paused, resumed
    assert_eq!(store.audit_count(id).await.expect("audit"), 3);
}

/// A progress line arriving while something else holds the database's write
/// lock waits its turn. It used to be refused at once ("database is locked"),
/// and the runner dropped the line. In-memory test hubs have one connection,
/// so this needs a real file.
#[tokio::test]
async fn an_event_waits_for_another_writer_instead_of_failing() {
    let dir = std::env::temp_dir().join(format!("decurion-busy-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).expect("dir");
    let url = format!("sqlite://{}", dir.join("hub.db").display());
    let store = Store::open(&url).await.expect("store");
    let r = router(AppState::new(
        store.clone(),
        Keys {
            owner: OWNER.into(),
            runner: RUNNER.into(),
        },
        LOGIN.into(),
        None,
    ));
    let id = seed(&r).await;
    claim(&r).await;

    // Another writer holds the lock for half a second.
    use sqlx::Connection;
    let mut other = sqlx::SqliteConnection::connect(&url)
        .await
        .expect("connect");
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut other)
        .await
        .expect("lock");
    sqlx::query("UPDATE settings SET value = value WHERE key = 'cloud_worker_seq'")
        .execute(&mut other)
        .await
        .expect("write");
    let started = std::time::Instant::now();
    let posting = tokio::spawn({
        let r = r.clone();
        async move {
            let answer = call(
                &r,
                "POST",
                &format!("/api/threads/{id}/events"),
                Some(RUNNER),
                json!({"kind":"message","text":"still here"}),
            )
            .await;
            (answer, started.elapsed())
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    sqlx::query("COMMIT")
        .execute(&mut other)
        .await
        .expect("unlock");

    let ((s, body), took) = posting.await.expect("task");
    assert_eq!(s, StatusCode::CREATED, "{body}");
    // It really met the lock and waited for it to be let go.
    assert!(took >= std::time::Duration::from_millis(400), "{took:?}");
    let (_, events) = call(
        &r,
        "GET",
        &format!("/api/threads/{id}/events"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert!(events.to_string().contains("still here"), "{events}");
    drop(other);
    drop(r);
    store.pool().close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every transaction that may write takes the write lock when it starts.
/// One that starts as a reader and then writes is refused at once whenever
/// another writer is busy; waiting does not help it. A plain `begin()` is
/// allowed only on a line marked `read-only`. This keeps new code from
/// bringing the refusal back.
#[test]
fn every_transaction_takes_the_write_lock_up_front() {
    fn scan(dir: &std::path::Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                scan(&path, found);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).expect("read");
                for (at, _) in text.match_indices(".begin()") {
                    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
                    let line_end = text[at..].find('\n').map_or(text.len(), |i| at + i);
                    let marked = text[line_start..line_end].contains("read-only");
                    let awaited = text[at + ".begin()".len()..]
                        .trim_start()
                        .starts_with(".await");
                    if awaited && !marked {
                        let line = text[..at].matches('\n').count() + 1;
                        found.push(format!("{}:{line}", path.display()));
                    }
                }
                if text.contains("query(\"BEGIN\")") || text.contains("begin_with(\"BEGIN\")") {
                    found.push(format!("{}: a bare BEGIN", path.display()));
                }
            }
        }
    }
    let mut found = Vec::new();
    scan(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut found,
    );
    assert!(
        found.is_empty(),
        "use begin_with(\"BEGIN IMMEDIATE\"), or mark a pure read `// read-only`: {found:?}"
    );
}

#[tokio::test]
async fn events_come_only_from_the_runner() {
    let (r, _) = hub().await;
    let id = seed(&r).await;
    claim(&r).await;
    let ev = json!({"kind":"message","text":"hello"});
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(OWNER),
        ev.clone(),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(RUNNER),
        ev,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (_, list) = call(
        &r,
        "GET",
        &format!("/api/threads/{id}/events"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(list[0]["event"]["text"], "hello");
}

#[tokio::test]
async fn audit_rows_cannot_be_changed() {
    let (r, store) = hub().await;
    let id = seed(&r).await;
    call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert!(sqlx::query("DELETE FROM audit")
        .execute(store.pool())
        .await
        .is_err());
    assert!(sqlx::query("UPDATE audit SET actor = 'x'")
        .execute(store.pool())
        .await
        .is_err());
    assert_eq!(store.audit_count(id).await.expect("audit"), 1);
}

#[tokio::test]
async fn login_code_opens_a_session_without_exposing_the_owner_key() {
    let (r, _) = hub().await;
    let resp = r
        .clone()
        .oneshot(
            Request::post("/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!("code={OWNER}")))
                .expect("req"),
        )
        .await
        .expect("resp");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let resp = r
        .clone()
        .oneshot(
            Request::post("/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!("code={LOGIN}")))
                .expect("req"),
        )
        .await
        .expect("resp");
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    assert!(!cookie.contains(OWNER) && !cookie.contains(LOGIN));
    let cookie_pair = cookie.split(';').next().unwrap_or_default().to_owned();
    let resp = r
        .clone()
        .oneshot(
            Request::get("/api/threads")
                .header(header::COOKIE, cookie_pair)
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);
    // A made-up session is refused.
    let resp = r
        .clone()
        .oneshot(
            Request::get("/api/threads")
                .header(header::COOKIE, "decurion_device=forged")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("resp");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn retry_of_work_that_never_started_starts_fresh() {
    let (r, _) = hub().await;
    let id = seed(&r).await;
    let (_, first) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    // Fails before the agent says anything (e.g. no token saved).
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(RUNNER),
        json!({"to":"failed"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(OWNER),
        json!({"to":"queued"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, again) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(again["resume"], false);
    assert_eq!(again["thread"]["session_id"], first["thread"]["session_id"]);
}

#[tokio::test]
async fn sending_back_from_review_requeues_for_the_runner() {
    let (r, _) = hub().await;
    let id = seed(&r).await;
    call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    call(
        &r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(RUNNER),
        json!({"kind":"message","text":"hi"}),
    )
    .await;
    call(
        &r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(RUNNER),
        json!({"to":"in_review"}),
    )
    .await;
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(OWNER),
        json!({"to":"running"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, t) = call(
        &r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(OWNER),
        json!({"to":"queued"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t["state"], "queued");
    let (s, again) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(again["resume"], true);
}

#[tokio::test]
async fn app_names_and_paths_are_checked() {
    let (r, _) = hub().await;
    for (name, path) in [
        ("..\\evil", "C:\\repos\\x"),
        ("Smith, Inc", "C:\\repos\\x"),
        ("ok", "--upload-pack=calc"),
        ("ok", "relative\\path"),
        ("ok", "C:\\repos\\a,b"),
    ] {
        let (s, _) = call(
            &r,
            "POST",
            "/api/apps",
            Some(OWNER),
            json!({"name":name,"repo_path":path,"profile":"A"}),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{name} {path}");
    }
    seed(&r).await;
    let (s, msg) = call(
        &r,
        "POST",
        "/api/apps",
        Some(OWNER),
        json!({"name":"demo","repo_path":"/repos/demo","profile":"B"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(msg.as_str().is_some_and(|m| m.contains("already exists")));
}

#[tokio::test]
async fn runner_cannot_write_owner_audit_rows() {
    let (r, store) = hub().await;
    let id = seed(&r).await;
    // A runner's name comes from its key, not from what it says.
    for name in ["owner", "Owner Key", "cloud-1"] {
        let (s, _) = call(
            &r,
            "POST",
            "/api/runner/claim",
            Some(RUNNER),
            json!({ "runner": name, "rules_version": decurion_proto::RULES_VERSION }),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{name}");
    }
    claim(&r).await;
    let actor: String = sqlx::query_scalar("SELECT actor FROM audit WHERE thread_id = ?")
        .bind(id)
        .fetch_one(store.pool())
        .await
        .expect("audit row");
    assert_eq!(actor, "runner:local");
}

#[tokio::test]
async fn board_sends_a_content_security_policy() {
    let (r, _) = hub().await;
    let resp = r
        .clone()
        .oneshot(Request::get("/").body(Body::empty()).expect("req"))
        .await
        .expect("resp");
    let csp = resp
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(csp.contains("frame-ancestors 'none'"));
}

#[tokio::test]
async fn the_plan_and_page_viewer_are_served_with_the_board_rules() {
    let (r, _) = hub().await;
    for path in ["/plan", "/pages/1/2"] {
        let resp = r
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).expect("req"))
            .await
            .expect("resp");
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
        let h = resp.headers();
        let csp = h
            .get(header::CONTENT_SECURITY_POLICY)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(csp.contains("default-src 'self'"), "{path}");
        assert!(csp.contains("frame-ancestors 'none'"), "{path}");
        assert_eq!(
            h.get(header::CACHE_CONTROL).and_then(|v| v.to_str().ok()),
            Some("no-store"),
            "{path}"
        );
    }
    // The viewer frames the agent's page with no permissions at all.
    let resp = r
        .clone()
        .oneshot(Request::get("/pages/1/2").body(Body::empty()).expect("req"))
        .await
        .expect("resp");
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains(r#"f.setAttribute("sandbox", "");"#));
    assert!(!html.contains("allow-scripts"));
}

async fn get(r: &axum::Router, path: &str, cookie: Option<&str>) -> axum::response::Response {
    let mut b = Request::get(path).header(header::USER_AGENT, "Mozilla/5.0 (Linux; Android 15)");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    r.clone()
        .oneshot(b.body(Body::empty()).expect("req"))
        .await
        .expect("resp")
}

async fn post_pair(r: &axum::Router, path: &str) -> axum::response::Response {
    let code = path.trim_start_matches("/pair?code=").to_owned();
    r.clone()
        .oneshot(
            Request::post("/pair")
                .header(header::USER_AGENT, "Mozilla/5.0 (Linux; Android 15)")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!("code={code}")))
                .expect("req"),
        )
        .await
        .expect("resp")
}

fn cookie_pair(resp: &axum::response::Response) -> String {
    resp.headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn a_phone_pairs_once_and_can_be_removed() {
    let (r, store) = hub().await;
    let (s, p) = call(&r, "POST", "/api/pairing", Some(OWNER), Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    let url = p["url"].as_str().expect("url").to_owned();
    assert!(url.starts_with("https://hub.test/pair?code="));
    assert!(p["qr_svg"].as_str().is_some_and(|q| q.contains("<svg")));
    let path = url.trim_start_matches("https://hub.test");

    // Opening the link (or a link preview fetching it) signs nothing in.
    let preview = get(&r, path, None).await;
    assert_eq!(preview.status(), StatusCode::OK);
    assert!(preview.headers().get(header::SET_COOKIE).is_none());
    let (_, devices) = call(&r, "GET", "/api/devices", Some(OWNER), Value::Null).await;
    assert_eq!(devices, json!([]));

    let resp = post_pair(&r, path).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/inbox")
    );
    let cookie = cookie_pair(&resp);
    assert!(cookie.starts_with("decurion_device="));

    // The code works only once.
    assert_eq!(post_pair(&r, path).await.status(), StatusCode::UNAUTHORIZED);

    // The phone can use the API, and survives a "restart" (state lives in the DB).
    assert_eq!(
        get(&r, "/api/threads", Some(&cookie)).await.status(),
        StatusCode::OK
    );
    let (_, devices) = call(&r, "GET", "/api/devices", Some(OWNER), Value::Null).await;
    assert_eq!(devices[0]["name"], "Android phone");
    let id = devices[0]["id"].as_i64().expect("id");

    // Only a hash of the token is stored.
    let token = cookie.trim_start_matches("decurion_device=");
    let stored: String = sqlx::query_scalar("SELECT token_hash FROM devices WHERE id = ?")
        .bind(id)
        .fetch_one(store.pool())
        .await
        .expect("row");
    assert_ne!(stored, token);
    assert_eq!(stored.len(), 64);

    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/devices/{id}/remove"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(
        get(&r, "/api/threads", Some(&cookie)).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn expired_pairing_codes_are_refused() {
    let (r, store) = hub().await;
    let (_, p) = call(&r, "POST", "/api/pairing", Some(OWNER), Value::Null).await;
    sqlx::query("UPDATE pairing_codes SET expires_at = '2000-01-01T00:00:00.000Z'")
        .execute(store.pool())
        .await
        .expect("expire");
    let path = p["url"]
        .as_str()
        .expect("url")
        .trim_start_matches("https://hub.test")
        .to_owned();
    assert_eq!(
        post_pair(&r, &path).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post_pair(&r, "/pair?code=abc123").await.status(),
        StatusCode::UNAUTHORIZED
    );
    // A link with markup in it is refused, not echoed into the page.
    assert_eq!(
        get(&r, "/pair?code=%3Cscript%3E", None).await.status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn only_the_owner_pairs_or_manages_devices() {
    let (r, _) = hub().await;
    let (s, _) = call(&r, "POST", "/api/pairing", Some(RUNNER), Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(&r, "GET", "/api/devices", Some(RUNNER), Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &r,
        "POST",
        "/api/devices/1/remove",
        Some(RUNNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_phone_sees_which_paired_device_is_itself() {
    let (r, _) = hub().await;
    let mut cookies = Vec::new();
    for _ in 0..2 {
        let (_, p) = call(&r, "POST", "/api/pairing", Some(OWNER), Value::Null).await;
        let path = p["url"]
            .as_str()
            .expect("url")
            .trim_start_matches("https://hub.test")
            .to_owned();
        cookies.push(cookie_pair(&post_pair(&r, &path).await));
    }
    let resp = get(&r, "/api/devices", Some(&cookies[1])).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let list: Value = serde_json::from_slice(&bytes).expect("json");
    let marks: Vec<bool> = list
        .as_array()
        .expect("list")
        .iter()
        .map(|d| d["this_device"].as_bool().expect("flag"))
        .collect();
    assert_eq!(marks, [false, true]);
    // The owner key is no device, so nothing is marked.
    let (_, list) = call(&r, "GET", "/api/devices", Some(OWNER), Value::Null).await;
    assert!(list
        .as_array()
        .expect("list")
        .iter()
        .all(|d| d["this_device"] == false && d["name"] == "Android phone"));
}

#[tokio::test]
async fn cookies_are_secure_behind_https() {
    let (r, _) = hub().await;
    let resp = r
        .clone()
        .oneshot(
            Request::post("/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("x-forwarded-proto", "https")
                .body(Body::from(format!("code={LOGIN}")))
                .expect("req"),
        )
        .await
        .expect("resp");
    let c = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(c.contains("; Secure") && c.contains("Max-Age="));
}

type Received = std::sync::Arc<tokio::sync::Mutex<Vec<(axum::http::HeaderMap, Vec<u8>)>>>;

#[tokio::test]
async fn notifications_are_encrypted_for_the_phone_and_dead_ones_dropped() {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use web_push_native::p256;

    // A pretend phone: its key pair and auth secret.
    let ua_secret = p256::SecretKey::from_slice(&[7u8; 32]).expect("key");
    let ua_public = ua_secret.public_key();
    let auth = [9u8; 16];

    // A pretend push service that records what it receives.
    let got: Received = Default::default();
    let sink = got.clone();
    let app = axum::Router::new()
        .route(
            "/ok",
            axum::routing::post(move |h: axum::http::HeaderMap, b: axum::body::Bytes| {
                let sink = sink.clone();
                async move {
                    sink.lock().await.push((h, b.to_vec()));
                    StatusCode::CREATED
                }
            }),
        )
        .route("/gone", axum::routing::post(|| async { StatusCode::GONE }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, app).await });

    let store = Store::open("sqlite::memory:").await.expect("store");
    let dir = std::env::temp_dir().join(format!("decurion-push-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let pusher = decurion_hub::push::Pusher::load_or_create(&dir).expect("pusher");
    assert_eq!(
        URL_SAFE_NO_PAD
            .decode(pusher.public_key())
            .expect("b64")
            .len(),
        65
    );

    let (phone, _) = store.add_device("Android phone").await.expect("device");
    let (old, _) = store.add_device("Old phone").await.expect("device");
    let p256dh = URL_SAFE_NO_PAD.encode(ua_public.to_sec1_bytes());
    let auth_b64 = URL_SAFE_NO_PAD.encode(auth);
    for (id, path) in [(phone.id, "ok"), (old.id, "gone")] {
        sqlx::query("INSERT INTO push_subscriptions (device_id, endpoint, p256dh, auth) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(format!("http://{addr}/{path}"))
            .bind(&p256dh)
            .bind(&auth_b64)
            .execute(store.pool())
            .await
            .expect("insert");
    }

    pusher
        .notify(&store, "Ready for review", "Add a hello script")
        .await;

    let got = got.lock().await;
    assert_eq!(got.len(), 1);
    let (headers, body) = &got[0];
    assert!(headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("vapid t=")));
    assert_eq!(
        headers
            .get("content-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("aes128gcm")
    );
    let plain = web_push_native::decrypt(
        body.clone(),
        &ua_secret,
        &web_push_native::Auth::clone_from_slice(&auth),
    )
    .expect("phone can decrypt");
    let note: Value = serde_json::from_slice(&plain).expect("json");
    assert_eq!(note["title"], "Ready for review");
    assert_eq!(note["body"], "Add a hello script");
    assert_eq!(note["url"], "/inbox");
    assert!(!String::from_utf8_lossy(body).contains("hello"));

    // The push service said the old phone is gone, so it was dropped.
    let left = store.push_targets().await.expect("targets");
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].0, phone.id);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn only_a_signed_in_device_can_subscribe() {
    let (r, _) = hub().await;
    let body = json!({"endpoint":"https://push.example/x","p256dh":"a","auth":"b"});
    let (s, _) = call(&r, "POST", "/api/push/subscribe", Some(OWNER), body.clone()).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(&r, "POST", "/api/push/subscribe", Some(RUNNER), body).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let resp = get(&r, "/sw.js", None).await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn opening_the_login_link_signs_nothing_in() {
    let (r, store) = hub().await;
    let resp = get(&r, &format!("/login?code={LOGIN}"), None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get(header::SET_COOKIE).is_none());
    assert!(store.devices().await.expect("devices").is_empty());
}

async fn post_login(r: &axum::Router, cookie: Option<&str>) -> axum::response::Response {
    let mut b = Request::post("/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("x-forwarded-proto", "https");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    r.clone()
        .oneshot(b.body(Body::from(format!("code={LOGIN}"))).expect("req"))
        .await
        .expect("resp")
}

#[tokio::test]
async fn a_signed_in_browser_opening_the_login_link_goes_straight_to_the_board() {
    let (r, store) = hub().await;
    let cookie = cookie_pair(&post_login(&r, None).await);
    assert_eq!(store.devices().await.expect("devices").len(), 1);

    let resp = get(&r, &format!("/login?code={LOGIN}"), Some(&cookie)).await;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/")
    );
    // The same sign-in, good for another year; no new device.
    let set = resp
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(set.starts_with(&format!("{cookie};")), "{set}");
    assert!(set.contains("Max-Age=31536000") && set.contains("HttpOnly"));
    assert!(set.contains("SameSite=Strict"));
    assert_eq!(store.devices().await.expect("devices").len(), 1);

    // A browser that was signed out sees the sign-in page as before.
    let id = store.devices().await.expect("devices")[0].id;
    store.remove_device(id).await.expect("remove");
    let resp = get(&r, &format!("/login?code={LOGIN}"), Some(&cookie)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get(header::SET_COOKIE).is_none());
}

#[tokio::test]
async fn signing_in_again_on_a_signed_in_browser_adds_no_device() {
    let (r, store) = hub().await;
    let cookie = cookie_pair(&post_login(&r, None).await);
    let again = post_login(&r, Some(&cookie)).await;
    assert_eq!(again.status(), StatusCode::SEE_OTHER);
    assert_eq!(cookie_pair(&again), cookie);
    assert_eq!(store.devices().await.expect("devices").len(), 1);
    // A cookie that no longer signs in gets a new device, as before.
    let fresh = post_login(&r, Some("decurion_device=forged")).await;
    assert_ne!(cookie_pair(&fresh), "decurion_device=forged");
    assert_eq!(store.devices().await.expect("devices").len(), 2);
}

#[tokio::test]
async fn a_device_in_use_gets_its_cookie_renewed() {
    let (r, store) = hub().await;
    let cookie = cookie_pair(&post_login(&r, None).await);
    let renewed = |resp: &axum::response::Response| {
        resp.headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    // The first use marks it as used, and renews the cookie for a year.
    let mut b = Request::get("/api/threads")
        .header(header::COOKIE, cookie.clone())
        .header("x-forwarded-proto", "https");
    let resp = r
        .clone()
        .oneshot(b.body(Body::empty()).expect("req"))
        .await
        .expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);
    let set = renewed(&resp).expect("renewed");
    assert!(set.starts_with(&format!("{cookie};")));
    assert!(set.contains("Max-Age=31536000") && set.contains("; Secure"));
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Strict"));
    // Polling within five minutes writes nothing and renews nothing.
    let resp = get(&r, "/api/threads", Some(&cookie)).await;
    assert_eq!(renewed(&resp), None);
    // Once its "last used" time is older, the next call renews it again.
    sqlx::query("UPDATE devices SET last_seen = '2000-01-01T00:00:00.000Z'")
        .execute(store.pool())
        .await
        .expect("age");
    b = Request::get("/api/threads").header(header::COOKIE, cookie.clone());
    let resp = r
        .clone()
        .oneshot(b.body(Body::empty()).expect("req"))
        .await
        .expect("resp");
    let set = renewed(&resp).expect("renewed again");
    assert!(set.starts_with(&format!("{cookie};")) && !set.contains("Secure"));
    // Key callers have no cookie to renew.
    let resp = r
        .clone()
        .oneshot(
            Request::get("/api/threads")
                .header(header::AUTHORIZATION, format!("Bearer {OWNER}"))
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("resp");
    assert_eq!(renewed(&resp), None);
}

#[tokio::test]
async fn cookie_writes_from_other_sites_are_refused() {
    let (r, store) = hub().await;
    let (_, token) = store
        .add_device("Browser on this PC")
        .await
        .expect("device");
    let cookie = format!("decurion_device={token}");
    let post = |site: Option<&'static str>, origin: Option<&'static str>| {
        let mut b = Request::post("/api/pairing")
            .header(header::COOKIE, cookie.clone())
            .header(header::HOST, "decurion.localhost:7420");
        if let Some(s) = site {
            b = b.header("sec-fetch-site", s);
        }
        if let Some(o) = origin {
            b = b.header(header::ORIGIN, o);
        }
        r.clone().oneshot(b.body(Body::empty()).expect("req"))
    };
    // Another app on the same host (different port) is "same-site", not "same-origin".
    assert_eq!(
        post(Some("same-site"), None).await.expect("resp").status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(Some("cross-site"), None).await.expect("resp").status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(None, Some("http://decurion.localhost:5173"))
            .await
            .expect("resp")
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(None, None).await.expect("resp").status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(Some("same-origin"), None)
            .await
            .expect("resp")
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        post(None, Some("http://decurion.localhost:7420"))
            .await
            .expect("resp")
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_new_pairing_code_retires_the_old_one() {
    let (r, _) = hub().await;
    let (_, first) = call(&r, "POST", "/api/pairing", Some(OWNER), Value::Null).await;
    let (_, second) = call(&r, "POST", "/api/pairing", Some(OWNER), Value::Null).await;
    let path = |p: &Value| {
        p["url"]
            .as_str()
            .expect("url")
            .trim_start_matches("https://hub.test")
            .to_owned()
    };
    assert_eq!(
        post_pair(&r, &path(&first)).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post_pair(&r, &path(&second)).await.status(),
        StatusCode::SEE_OTHER
    );
}

#[tokio::test]
async fn the_inbox_can_ask_for_the_latest_events() {
    let (r, _) = hub().await;
    let id = seed(&r).await;
    claim(&r).await;
    for i in 0..30 {
        call(
            &r,
            "POST",
            &format!("/api/threads/{id}/events"),
            Some(RUNNER),
            json!({"kind":"message","text":format!("m{i}")}),
        )
        .await;
    }
    let (_, tail) = call(
        &r,
        "GET",
        &format!("/api/threads/{id}/events?tail=3"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    let texts: Vec<&str> = tail
        .as_array()
        .expect("list")
        .iter()
        .filter_map(|e| e["event"]["text"].as_str())
        .collect();
    assert_eq!(texts, ["m27", "m28", "m29"]);
}

#[tokio::test]
async fn a_signed_in_phone_survives_a_hub_restart() {
    let dir = std::env::temp_dir().join(format!("decurion-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let url = format!("sqlite://{}", dir.join("hub.db").display());
    let token = {
        let store = Store::open(&url).await.expect("store");
        let code = store.new_pairing_code().await.expect("code");
        store
            .pair_device(&code, "Android phone")
            .await
            .expect("pair")
            .expect("token")
    };
    // A brand-new hub on the same database, with a new login code.
    let store = Store::open(&url).await.expect("reopen");
    let r = router(AppState::new(
        store,
        Keys {
            owner: OWNER.into(),
            runner: RUNNER.into(),
        },
        "fedcba987654".into(),
        None,
    ));
    let resp = get(
        &r,
        "/api/threads",
        Some(&format!("decurion_device={token}")),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    drop(r);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn re_pairing_a_browser_moves_its_notifications() {
    let (_, store) = hub().await;
    let (old, _) = store.add_device("Android phone").await.expect("old");
    let (new, _) = store.add_device("Android phone").await.expect("new");
    let sub = decurion_hub::devices::PushSubscription {
        endpoint: "https://push.example/abc".into(),
        p256dh: "k".into(),
        auth: "a".into(),
    };
    store.save_push(old.id, &sub).await.expect("old sub");
    store.save_push(new.id, &sub).await.expect("new sub");
    let targets = store.push_targets().await.expect("targets");
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].0, new.id);
    // A stale "gone" for an older address does not remove the current one.
    store
        .drop_push(new.id, "https://push.example/older")
        .await
        .expect("drop");
    assert_eq!(store.push_targets().await.expect("targets").len(), 1);
}

async fn ppu_app(r: &axum::Router) -> i64 {
    let (s, app) = call(
        r,
        "POST",
        "/api/apps",
        Some(OWNER),
        json!({"name":"paid","repo_path":"C:\\repos\\paid","profile":"A","pay_per_use":true}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(app["pay_per_use"], true);
    app["id"].as_i64().expect("id")
}

async fn add_work(r: &axum::Router, app: i64, title: &str) -> i64 {
    let (_, t) = call(
        r,
        "POST",
        "/api/threads",
        Some(OWNER),
        json!({"app_id": app, "title": title, "prompt": "p"}),
    )
    .await;
    t["id"].as_i64().expect("id")
}

async fn finish(r: &axum::Router, id: i64, cost: Option<f64>) {
    if let Some(c) = cost {
        call(
            r,
            "POST",
            &format!("/api/threads/{id}/events"),
            Some(RUNNER),
            json!({"kind":"message","text":"working"}),
        )
        .await;
        call(
            r,
            "POST",
            &format!("/api/threads/{id}/events"),
            Some(RUNNER),
            json!({"kind":"finished","ok":true,"summary":"done","cost_usd":c}),
        )
        .await;
    }
    let (s, _) = call(
        r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(RUNNER),
        json!({"to":"in_review"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

async fn spend(r: &axum::Router) -> Value {
    call(r, "GET", "/api/spend", Some(OWNER), Value::Null)
        .await
        .1
}

#[tokio::test]
async fn pay_per_use_work_sets_money_aside_and_stops_at_the_cap() {
    let (r, store) = hub().await;
    let app = ppu_app(&r).await;
    let (s, _) = call(
        &r,
        "POST",
        "/api/spend/cap",
        Some(OWNER),
        json!({"cap_usd": 12.0}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let a = add_work(&r, app, "first").await;
    let b = add_work(&r, app, "second").await;
    let c = add_work(&r, app, "third").await;

    let (_, first) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(first["thread"]["id"], a);
    // $10 set aside; Claude Code gets 90% of it as its own limit.
    assert_eq!(first["budget_usd"], 9.0);
    // While it runs, its whole budget counts against the month.
    assert_eq!(spend(&r).await["pay_per_use_usd"], 10.0);
    finish(&r, a, Some(3.25)).await;
    assert_eq!(spend(&r).await["pay_per_use_usd"], 3.25);

    let (_, second) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(second["thread"]["id"], b);
    // $8.75 left and set aside; 90% rounded down to the cent.
    assert_eq!(second["budget_usd"], 7.87);
    finish(&r, b, Some(8.6)).await;

    // $0.15 left is under the $0.50 minimum: the third waits on the owner.
    let (s, _) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let t = store.thread(c).await.expect("thread");
    assert_eq!(t.state, decurion_proto::ThreadState::NeedsYou);
    let note: String =
        sqlx::query_scalar("SELECT note FROM audit WHERE thread_id = ? ORDER BY id DESC LIMIT 1")
            .bind(c)
            .fetch_one(store.pool())
            .await
            .expect("audit");
    assert!(note.contains("cap of $12.00 reached"));
    let sp = spend(&r).await;
    assert!((sp["pay_per_use_usd"].as_f64().unwrap_or(0.0) - 11.85).abs() < 1e-9);
}

#[tokio::test]
async fn unknown_cost_counts_as_spent_only_if_the_agent_ran() {
    let (r, store) = hub().await;
    let app = ppu_app(&r).await;
    let a = add_work(&r, app, "never started").await;
    call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    // Failed before the agent said anything (e.g. no key saved).
    call(
        &r,
        "POST",
        &format!("/api/threads/{a}/move"),
        Some(RUNNER),
        json!({"to":"failed"}),
    )
    .await;
    assert_eq!(spend(&r).await["pay_per_use_usd"], 0.0);

    let b = add_work(&r, app, "died mid-run").await;
    call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    call(
        &r,
        "POST",
        &format!("/api/threads/{b}/events"),
        Some(RUNNER),
        json!({"kind":"tool","name":"Bash","summary":"ls"}),
    )
    .await;
    // The runner restarts without a cost report: assume the budget was spent.
    let (s, paused) = call(
        &r,
        "POST",
        "/api/runner/hello",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(paused["paused"], json!([b]));
    assert_eq!(spend(&r).await["pay_per_use_usd"], 10.0);
    // The run was really closed and charged, not just left open.
    let (open, cost): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*) FILTER (WHERE finished_at IS NULL), COALESCE(SUM(cost_micros), 0)
         FROM runs WHERE thread_id = ?",
    )
    .bind(b)
    .fetch_one(store.pool())
    .await
    .expect("runs");
    assert_eq!((open, cost), (0, 10_000_000));
}

#[tokio::test]
async fn max_account_runs_have_no_budget_and_show_as_usage() {
    let (r, _) = hub().await;
    let id = seed(&r).await;
    let (_, claim) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert!(claim["budget_usd"].is_null());
    finish(&r, id, Some(1.5)).await;
    let sp = spend(&r).await;
    assert_eq!(sp["pay_per_use_usd"], 0.0);
    assert_eq!(sp["max_equivalent_usd"], 1.5);
    assert_eq!(sp["cap_usd"], 100.0);
}

#[tokio::test]
async fn last_months_spending_does_not_count() {
    let (r, store) = hub().await;
    let app = ppu_app(&r).await;
    let a = add_work(&r, app, "old").await;
    call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    finish(&r, a, Some(7.0)).await;
    sqlx::query("UPDATE runs SET started_at = '2000-01-15T00:00:00.000Z'")
        .execute(store.pool())
        .await
        .expect("age");
    assert_eq!(spend(&r).await["pay_per_use_usd"], 0.0);
}

#[tokio::test]
async fn only_the_owner_changes_money_settings() {
    let (r, _) = hub().await;
    let app = ppu_app(&r).await;
    let (s, _) = call(
        &r,
        "POST",
        "/api/spend/cap",
        Some(RUNNER),
        json!({"cap_usd": 1000.0}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/apps/{app}/pay-per-use"),
        Some(RUNNER),
        json!({"on": false}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &r,
        "POST",
        "/api/spend/cap",
        Some(OWNER),
        json!({"cap_usd": -5.0}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/apps/{app}/pay-per-use"),
        Some(OWNER),
        json!({"on": false}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn a_resumed_session_is_charged_only_for_what_it_added() {
    let (r, _) = hub().await;
    let app = ppu_app(&r).await;
    let id = add_work(&r, app, "long job").await;
    call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    call(
        &r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(RUNNER),
        json!({"kind":"message","text":"working"}),
    )
    .await;
    call(
        &r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(RUNNER),
        json!({"kind":"finished","ok":false,"summary":"stopped","cost_usd":4.0}),
    )
    .await;
    // The owner sends it back; it resumes the same Claude session.
    call(
        &r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(RUNNER),
        json!({"to":"needs_you"}),
    )
    .await;
    call(
        &r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(OWNER),
        json!({"to":"queued"}),
    )
    .await;
    let (_, again) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(again["resume"], true);
    // Claude Code counts the session from the start: $4 already, plus 90% of $10.
    assert_eq!(again["budget_usd"], 13.0);
    // It reports $6.50 for the whole session, so this run cost $2.50.
    finish(&r, id, Some(6.5)).await;
    assert_eq!(spend(&r).await["pay_per_use_usd"], 6.5);
}

#[tokio::test]
async fn capped_pay_per_use_work_does_not_hold_up_max_work() {
    let (r, store) = hub().await;
    let app = ppu_app(&r).await;
    call(
        &r,
        "POST",
        "/api/spend/cap",
        Some(OWNER),
        json!({"cap_usd": 0.0}),
    )
    .await;
    let a = add_work(&r, app, "paid one").await;
    let b = add_work(&r, app, "paid two").await;
    let max = seed(&r).await;
    let (s, claim) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(claim["thread"]["id"], max);
    for id in [a, b] {
        assert_eq!(
            store.thread(id).await.expect("t").state,
            decurion_proto::ThreadState::NeedsYou
        );
    }
}

#[tokio::test]
async fn an_absurd_cost_report_cannot_break_the_totals() {
    let (r, _) = hub().await;
    let app = ppu_app(&r).await;
    let id = add_work(&r, app, "x").await;
    call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    finish(&r, id, Some(1e300)).await;
    let (s, sp) = call(&r, "GET", "/api/spend", Some(OWNER), Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(sp["pay_per_use_usd"], 1000.0);
    // Claims still work afterwards.
    seed(&r).await;
    let (s, _) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn a_started_box_without_a_trusted_cost_is_charged_its_budget() {
    let (r, _) = hub().await;
    let app = ppu_app(&r).await;
    let id = add_work(&r, app, "x").await;
    call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    call(
        &r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(RUNNER),
        json!({"kind":"started"}),
    )
    .await;
    // Finished arrives without a cost (the runner did not trust it).
    call(
        &r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(RUNNER),
        json!({"kind":"finished","ok":false,"summary":"?"}),
    )
    .await;
    call(
        &r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(RUNNER),
        json!({"to":"failed"}),
    )
    .await;
    assert_eq!(spend(&r).await["pay_per_use_usd"], 10.0);
}

#[tokio::test]
async fn pages_load_their_shared_look_without_signing_in() {
    let (r, _) = hub().await;
    for (path, kind) in [
        ("/ui.css", "text/css"),
        ("/ui.js", "text/javascript"),
        ("/compose.js", "text/javascript"),
        ("/settings.js", "text/javascript"),
        ("/voice.js", "text/javascript"),
        ("/icon.svg", "image/svg+xml"),
        ("/icon-192.png", "image/png"),
        ("/icon-512.png", "image/png"),
    ] {
        let resp = r
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).expect("req"))
            .await
            .expect("resp");
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(ct.starts_with(kind), "{path}: {ct}");
    }
}

async fn app_on(r: &axum::Router, name: &str, profile: &str) -> i64 {
    let (s, app) = call(
        r,
        "POST",
        "/api/apps",
        Some(OWNER),
        json!({"name":name,"repo_path":"C:\\repos\\x","profile":profile}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    app["id"].as_i64().expect("id")
}

async fn claim_id(r: &axum::Router) -> Option<i64> {
    let (s, c) = call(
        r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    match s {
        StatusCode::OK => Some(c["thread"]["id"].as_i64().expect("id")),
        StatusCode::NO_CONTENT => None,
        other => panic!("claim failed: {other} {c}"),
    }
}

/// The runner's side of a run that hits the Max limit: it reports the limit,
/// and the hub requeues the work in the same step.
async fn hit_limit(r: &axum::Router, id: i64, reset_epoch: Option<i64>) {
    call(
        r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(RUNNER),
        json!({"kind":"started"}),
    )
    .await;
    call(
        r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(RUNNER),
        json!({"kind":"message","text":"working"}),
    )
    .await;
    let ev = match reset_epoch {
        Some(e) => json!({"kind":"limit_hit","reset_epoch":e}),
        None => json!({"kind":"limit_hit"}),
    };
    let (s, _) = call(
        r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(RUNNER),
        ev,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (_, t) = call(
        r,
        "GET",
        &format!("/api/threads/{id}"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(t["state"], "queued", "requeued by the hub with the limit");
}

#[tokio::test]
async fn work_on_an_exhausted_max_account_waits_while_other_work_runs() {
    let (r, _) = hub().await;
    let a = app_on(&r, "on-a", "A").await;
    let b = app_on(&r, "on-b", "B").await;
    let a1 = add_work(&r, a, "a1").await;
    let a2 = add_work(&r, a, "a2").await;
    let b1 = add_work(&r, b, "b1").await;

    assert_eq!(claim_id(&r).await, Some(a1));
    hit_limit(&r, a1, None).await;

    let (_, limits) = call(&r, "GET", "/api/limits", Some(OWNER), Value::Null).await;
    assert_eq!(limits[0]["profile"], "A");
    // Both jobs on A are queued and waiting (the one that hit it is back in line).
    assert_eq!(limits[0]["waiting"], 2);

    // A's work waits; B's runs.
    assert_eq!(claim_id(&r).await, Some(b1));
    assert_eq!(claim_id(&r).await, None);
    let _ = a2;
}

#[tokio::test]
async fn one_tap_moves_waiting_work_to_pay_per_use() {
    let (r, store) = hub().await;
    let a = app_on(&r, "on-a", "A").await;
    let a1 = add_work(&r, a, "a1").await;
    let a2 = add_work(&r, a, "a2").await;
    assert_eq!(claim_id(&r).await, Some(a1));
    hit_limit(&r, a1, None).await;
    assert_eq!(claim_id(&r).await, None);

    let (s, _) = call(
        &r,
        "POST",
        "/api/limits/A/pay-per-use",
        Some(RUNNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, moved) = call(
        &r,
        "POST",
        "/api/limits/A/pay-per-use",
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    // Nothing waits now: the account runs on pay-per-use until its reset.
    assert_eq!(moved["waiting"], 0);
    assert_eq!(moved["pay_per_use_until"], moved["until"]);

    // Both now run on pay-per-use, with a budget, under the cap; a1 resumes.
    let (_, c) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(c["thread"]["id"], a1);
    assert_eq!(c["resume"], true);
    assert!(c["budget_usd"].as_f64().is_some_and(|b| b > 0.0));
    let account: String =
        sqlx::query_scalar("SELECT account FROM runs WHERE thread_id = ? ORDER BY id DESC LIMIT 1")
            .bind(a1)
            .fetch_one(store.pool())
            .await
            .expect("run");
    assert_eq!(account, "api");
    assert_eq!(claim_id(&r).await, Some(a2));
}

#[tokio::test]
async fn one_tap_keeps_the_plan_on_pay_per_use_until_it_resets() {
    let (r, store) = hub().await;
    let a = app_on(&r, "on-a", "A").await;
    let a1 = add_work(&r, a, "a1").await;
    assert_eq!(claim_id(&r).await, Some(a1));
    hit_limit(&r, a1, None).await;
    let (s, l) = call(
        &r,
        "POST",
        "/api/limits/A/pay-per-use",
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, limits) = call(&r, "GET", "/api/limits", Some(OWNER), Value::Null).await;
    assert_eq!(limits[0], l, "the board sees the same");
    assert!(l["pay_per_use_until"].is_string());
    let c = claim(&r).await;
    assert_eq!(
        (c["thread"]["id"].as_i64(), &c["account"]),
        (Some(a1), &json!("api"))
    );

    // Work added after the tap runs on pay-per-use too, with its own budget.
    let a2 = add_work(&r, a, "a2").await;
    let c = claim(&r).await;
    assert_eq!(c["thread"]["id"], a2);
    assert_eq!(c["account"], "api");
    assert!(c["budget_usd"].as_f64().is_some_and(|b| b > 0.0));

    // After the reset time, work is back on the Max plan by itself.
    sqlx::query(
        "UPDATE profile_limits SET until = '2000-01-01T00:00:00.000Z',
             pay_per_use_until = '2000-01-01T00:00:00.000Z'",
    )
    .execute(store.pool())
    .await
    .expect("age");
    let a3 = add_work(&r, a, "a3").await;
    let c = claim(&r).await;
    assert_eq!(c["thread"]["id"], a3);
    assert_eq!(c["account"], "A");
    assert!(c["budget_usd"].is_null());

    // A later outage waits again until the owner taps again.
    hit_limit(&r, a3, None).await;
    let (_, limits) = call(&r, "GET", "/api/limits", Some(OWNER), Value::Null).await;
    assert_eq!(limits[0]["waiting"], 1);
    assert!(limits[0]["pay_per_use_until"].is_null());
    assert_eq!(claim_id(&r).await, None);
}

#[tokio::test]
async fn the_pay_per_use_tap_after_the_reset_does_nothing() {
    let (r, store) = hub().await;
    let a = app_on(&r, "on-a", "A").await;
    let a1 = add_work(&r, a, "a1").await;
    assert_eq!(claim_id(&r).await, Some(a1));
    hit_limit(&r, a1, None).await;
    sqlx::query("UPDATE profile_limits SET until = '2000-01-01T00:00:00.000Z'")
        .execute(store.pool())
        .await
        .expect("age");
    // A stale banner tapped after the reset.
    let (s, l) = call(
        &r,
        "POST",
        "/api/limits/A/pay-per-use",
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(l.is_null(), "{l}");
    let set: Option<String> = sqlx::query_scalar("SELECT pay_per_use_until FROM profile_limits")
        .fetch_one(store.pool())
        .await
        .expect("row");
    assert_eq!(set, None);
    assert_eq!(claim(&r).await["account"], "A");
}

#[tokio::test]
async fn waiting_work_restarts_on_its_own_after_the_reset() {
    let (r, store) = hub().await;
    let a = app_on(&r, "on-a", "A").await;
    let a1 = add_work(&r, a, "a1").await;
    assert_eq!(claim_id(&r).await, Some(a1));
    hit_limit(&r, a1, None).await;
    assert_eq!(claim_id(&r).await, None);
    // Time passes beyond the reset.
    sqlx::query("UPDATE profile_limits SET until = '2000-01-01T00:00:00.000Z'")
        .execute(store.pool())
        .await
        .expect("age");
    assert_eq!(claim_id(&r).await, Some(a1));
    let (_, limits) = call(&r, "GET", "/api/limits", Some(OWNER), Value::Null).await;
    assert_eq!(limits, json!([]));
}

#[tokio::test]
async fn a_believable_reset_time_is_used_and_a_silly_one_is_not() {
    let (r, store) = hub().await;
    let a = app_on(&r, "on-a", "A").await;
    let a1 = add_work(&r, a, "a1").await;
    assert_eq!(claim_id(&r).await, Some(a1));
    let in_two_hours: i64 =
        sqlx::query_scalar("SELECT CAST(strftime('%s','now','+2 hours') AS INTEGER)")
            .fetch_one(store.pool())
            .await
            .expect("now");
    hit_limit(&r, a1, Some(in_two_hours)).await;
    let until: String = sqlx::query_scalar("SELECT until FROM profile_limits WHERE profile = 'A'")
        .fetch_one(store.pool())
        .await
        .expect("limit");
    let expect: String =
        sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', ?, 'unixepoch')")
            .bind(in_two_hours)
            .fetch_one(store.pool())
            .await
            .expect("fmt");
    assert_eq!(until, expect);

    // A reset a year away falls back to an hour.
    let a2 = add_work(&r, a, "a2").await;
    sqlx::query("DELETE FROM profile_limits")
        .execute(store.pool())
        .await
        .expect("clear");
    let _ = claim_id(&r).await; // a1 resumes
    call(
        &r,
        "POST",
        &format!("/api/threads/{a1}/move"),
        Some(RUNNER),
        json!({"to":"in_review"}),
    )
    .await;
    assert_eq!(claim_id(&r).await, Some(a2));
    hit_limit(&r, a2, Some(in_two_hours + 365 * 86_400)).await;
    let soon: bool = sqlx::query_scalar(
        "SELECT until < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+61 minutes') FROM profile_limits WHERE profile = 'A'",
    )
    .fetch_one(store.pool())
    .await
    .expect("limit");
    assert!(soon);
}

#[tokio::test]
async fn a_limit_on_a_pay_per_use_run_does_not_block_a_max_account() {
    let (r, _) = hub().await;
    let app = ppu_app(&r).await;
    let id = add_work(&r, app, "paid").await;
    assert_eq!(claim_id(&r).await, Some(id));
    call(
        &r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(RUNNER),
        json!({"kind":"limit_hit"}),
    )
    .await;
    // Not a Max limit: nothing recorded, and the run is left to the runner.
    let (_, t) = call(
        &r,
        "GET",
        &format!("/api/threads/{id}"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(t["state"], "running");
    let (_, limits) = call(&r, "GET", "/api/limits", Some(OWNER), Value::Null).await;
    assert_eq!(limits, json!([]));
}

#[tokio::test]
async fn the_pay_per_use_tap_does_nothing_when_no_account_is_out() {
    let (r, _) = hub().await;
    let a = app_on(&r, "on-a", "A").await;
    add_work(&r, a, "a1").await;
    let (s, moved) = call(
        &r,
        "POST",
        "/api/limits/A/pay-per-use",
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(moved.is_null(), "{moved}");
    let (_, c) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert!(c["budget_usd"].is_null());
}

#[tokio::test]
async fn pay_per_use_from_an_outage_ends_when_the_account_is_back() {
    let (r, store) = hub().await;
    let a = app_on(&r, "on-a", "A").await;
    let a1 = add_work(&r, a, "a1").await;
    assert_eq!(claim_id(&r).await, Some(a1));
    hit_limit(&r, a1, None).await;
    call(
        &r,
        "POST",
        "/api/limits/A/pay-per-use",
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(claim_id(&r).await, Some(a1));
    call(
        &r,
        "POST",
        &format!("/api/threads/{a1}/move"),
        Some(RUNNER),
        json!({"to":"in_review"}),
    )
    .await;
    // The account comes back; the owner sends the work back for changes.
    sqlx::query("DELETE FROM profile_limits")
        .execute(store.pool())
        .await
        .expect("clear");
    call(
        &r,
        "POST",
        &format!("/api/threads/{a1}/move"),
        Some(OWNER),
        json!({"to":"queued"}),
    )
    .await;
    let (_, c) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(c["thread"]["id"], a1);
    assert!(c["budget_usd"].is_null(), "back on the Max account");
}

#[tokio::test]
async fn repeat_hits_in_one_outage_do_not_alert_again_and_never_shorten_it() {
    let (r, store) = hub().await;
    let a = app_on(&r, "on-a", "A").await;
    let a1 = add_work(&r, a, "a1").await;
    assert_eq!(claim_id(&r).await, Some(a1));
    // A second piece of work on the same account, running at the same time,
    // runs into the same outage.
    let a2 = add_work(&r, a, "a2").await;
    assert_eq!(claim_id(&r).await, Some(a2));
    for t in [a1, a2] {
        call(
            &r,
            "POST",
            &format!("/api/threads/{t}/events"),
            Some(RUNNER),
            json!({"kind":"started"}),
        )
        .await;
    }
    let later: i64 = sqlx::query_scalar("SELECT CAST(strftime('%s','now','+3 hours') AS INTEGER)")
        .fetch_one(store.pool())
        .await
        .expect("t");
    let (_, first) = store
        .add_event(
            a1,
            "local",
            &decurion_proto::RunEvent::LimitHit {
                reset_epoch: Some(later),
            },
        )
        .await
        .expect("first");
    assert!(first, "a new outage alerts");
    let (_, again) = store
        .add_event(
            a2,
            "local",
            &decurion_proto::RunEvent::LimitHit { reset_epoch: None },
        )
        .await
        .expect("again");
    assert!(!again, "the same outage does not alert twice");
    // The later reset time stands; the default hour does not shorten it.
    let still_later: bool = sqlx::query_scalar(
        "SELECT until > strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+2 hours') FROM profile_limits",
    )
    .fetch_one(store.pool())
    .await
    .expect("until");
    assert!(still_later);
}

#[tokio::test]
async fn a_reset_time_in_a_few_seconds_still_waits_five_minutes() {
    let (r, store) = hub().await;
    let a = app_on(&r, "on-a", "A").await;
    let a1 = add_work(&r, a, "a1").await;
    assert_eq!(claim_id(&r).await, Some(a1));
    let soon: i64 =
        sqlx::query_scalar("SELECT CAST(strftime('%s','now','+10 seconds') AS INTEGER)")
            .fetch_one(store.pool())
            .await
            .expect("t");
    hit_limit(&r, a1, Some(soon)).await;
    let waits: bool = sqlx::query_scalar(
        "SELECT until >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+4 minutes') FROM profile_limits",
    )
    .fetch_one(store.pool())
    .await
    .expect("until");
    assert!(waits);
    assert_eq!(claim_id(&r).await, None);
}

async fn add_app_with_sites(r: &axum::Router, sites: Value) -> (StatusCode, Value) {
    call(
        r,
        "POST",
        "/api/apps",
        Some(OWNER),
        json!({"name":"web","repo_path":"C:\\repos\\web","profile":"A","sites":sites}),
    )
    .await
}

#[tokio::test]
async fn an_apps_websites_are_cleaned_up_when_added() {
    let (r, _) = hub().await;
    let (s, app) = add_app_with_sites(
        &r,
        json!([
            "https://Stripe.com/docs",
            "stripe.com",
            "*.docs.rs",
            "pypi.org",
            "https://api.anthropic.com/v1",
            " ",
        ]),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(app["sites"], json!(["stripe.com", "docs.rs"]));
    let (_, apps) = call(&r, "GET", "/api/apps", Some(OWNER), Value::Null).await;
    assert_eq!(apps[0]["sites"], json!(["stripe.com", "docs.rs"]));
    // Apps added without a list get the built-in sites only.
    seed(&r).await;
    let (_, apps) = call(&r, "GET", "/api/apps", Some(OWNER), Value::Null).await;
    assert_eq!(apps[0]["name"], "demo");
    assert_eq!(apps[0]["sites"], json!([]));
}

#[tokio::test]
async fn a_bad_website_is_refused_by_name() {
    let (r, _) = hub().await;
    for bad in ["10.0.0.1", "com", "*.com", "not a site"] {
        let (s, msg) = add_app_with_sites(&r, json!(["stripe.com", bad])).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
        assert!(msg.as_str().is_some_and(|m| m.contains(bad)), "{msg}");
    }
    let too_many: Vec<String> = (0..=decurion_proto::sites::MAX_SITES)
        .map(|i| format!("site{i}.example.com"))
        .collect();
    let (s, _) = add_app_with_sites(&r, json!(too_many)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, apps) = call(&r, "GET", "/api/apps", Some(OWNER), Value::Null).await;
    assert_eq!(apps, json!([]));
}

#[tokio::test]
async fn the_owner_changes_an_apps_websites_and_the_next_run_gets_them() {
    let (r, _) = hub().await;
    let id = seed(&r).await;
    let path = "/api/apps/1/sites";
    let (s, stored) = call(
        &r,
        "POST",
        path,
        Some(OWNER),
        json!({"sites":["https://GitHub.com/x", "crates.io", "github.com"]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(stored, json!(["github.com"]));
    let (_, apps) = call(&r, "GET", "/api/apps", Some(OWNER), Value::Null).await;
    assert_eq!(apps[0]["sites"], json!(["github.com"]));
    let (s, c) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(c["thread"]["id"], id);
    assert_eq!(c["app"]["sites"], json!(["github.com"]));

    // A bad entry changes nothing.
    let (s, _) = call(
        &r,
        "POST",
        path,
        Some(OWNER),
        json!({"sites":["github.com", "x"]}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, apps) = call(&r, "GET", "/api/apps", Some(OWNER), Value::Null).await;
    assert_eq!(apps[0]["sites"], json!(["github.com"]));
    // Clearing the list is allowed.
    let (s, stored) = call(&r, "POST", path, Some(OWNER), json!({"sites":[]})).await;
    assert_eq!((s, stored), (StatusCode::OK, json!([])));
    let (s, _) = call(
        &r,
        "POST",
        "/api/apps/99/sites",
        Some(OWNER),
        json!({"sites":["github.com"]}),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn only_the_owner_chooses_an_apps_websites() {
    let (r, _) = hub().await;
    seed(&r).await;
    let (s, msg) = call(
        &r,
        "POST",
        "/api/apps/1/sites",
        Some(RUNNER),
        json!({"sites":["evil.example"]}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(msg.as_str().is_some_and(|m| m.contains("owner-only")));
    let (_, apps) = call(&r, "GET", "/api/apps", Some(OWNER), Value::Null).await;
    assert_eq!(apps[0]["sites"], json!([]));
    // Anyone signed in can see the built-in list; nobody else can.
    let (s, built_in) = call(&r, "GET", "/api/sites/built-in", Some(RUNNER), Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(built_in, json!(decurion_proto::sites::BUILT_IN));
    let (s, _) = call(&r, "GET", "/api/sites/built-in", None, Value::Null).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_blocked_website_is_logged_and_counts_as_the_box_running() {
    let (r, _) = hub().await;
    let app = ppu_app(&r).await;
    let id = add_work(&r, app, "x").await;
    call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    let blocked = json!({"kind":"blocked","host":"evil.example","reason":"not on this app's list"});
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(RUNNER),
        blocked.clone(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(OWNER),
        blocked.clone(),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (_, ev) = call(
        &r,
        "GET",
        &format!("/api/threads/{id}/events"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(ev.as_array().map(Vec::len), Some(1));
    assert_eq!(ev[0]["event"], blocked);

    // The runner dies: the box was up and reaching out, so the budget counts
    // as spent, but no agent session was saved yet, so the retry starts fresh.
    call(
        &r,
        "POST",
        "/api/runner/hello",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(spend(&r).await["pay_per_use_usd"], 10.0);
    let (_, c) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(c["thread"]["id"], id);
    assert_eq!(c["resume"], false);
}

/// A hub with its own folder vault, removed when the test is done.
struct KeyHub {
    r: axum::Router,
    store: Store,
    vault: decurion_vault::Vault,
    dir: std::path::PathBuf,
}

impl Drop for KeyHub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn key_hub() -> KeyHub {
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let dir = std::env::temp_dir().join(format!(
        "decurion-hub-vault-{}",
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
            LOGIN.into(),
            None,
        )
        .with_vault(vault.clone()),
    );
    KeyHub {
        r,
        store,
        vault,
        dir,
    }
}

async fn demo_app(r: &axum::Router) -> i64 {
    let (s, app) = call(
        r,
        "POST",
        "/api/apps",
        Some(OWNER),
        json!({"name":"demo","repo_path":"C:\\repos\\demo","profile":"A"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    app["id"].as_i64().expect("id")
}

async fn add_key(r: &axum::Router, app: i64, name: &str, value: &str) -> (StatusCode, Value) {
    call(
        r,
        "POST",
        &format!("/api/apps/{app}/test-keys"),
        Some(OWNER),
        json!({"name": name, "value": value}),
    )
    .await
}

async fn list_keys(r: &axum::Router, app: i64) -> Value {
    let (s, v) = call(
        r,
        "GET",
        &format!("/api/apps/{app}/test-keys"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    v
}

/// Everything the database holds about test keys, as one string.
async fn key_rows(store: &Store) -> String {
    let rows: Vec<(i64, String, String, String)> =
        sqlx::query_as("SELECT app_id, name, last_four, added_at FROM test_keys ORDER BY name")
            .fetch_all(store.pool())
            .await
            .expect("rows");
    format!("{rows:?}")
}

#[tokio::test]
async fn test_keys_are_added_listed_replaced_and_removed() {
    let h = key_hub().await;
    let app = demo_app(&h.r).await;
    assert_eq!(list_keys(&h.r, app).await, json!([]));

    let first = "sk_test_51Habc123def456";
    let (s, k) = add_key(&h.r, app, "STRIPE_SECRET_KEY", first).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(k["name"], "STRIPE_SECRET_KEY");
    assert_eq!(k["last_four"], "f456");
    assert!(!k.to_string().contains(first));
    let (s, _) = add_key(
        &h.r,
        app,
        "SUPABASE_ANON_KEY",
        "eyJhbGciOiJIUzI1NiJ9.anon9999",
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);

    let list = list_keys(&h.r, app).await;
    assert_eq!(list[0]["name"], "STRIPE_SECRET_KEY");
    assert_eq!(list[1]["name"], "SUPABASE_ANON_KEY");
    assert_eq!(list[1]["last_four"], "9999");
    assert!(!list.to_string().contains(first));
    // The value is in the vault and nowhere in the database.
    assert_eq!(
        h.vault
            .get(app, "STRIPE_SECRET_KEY")
            .ok()
            .flatten()
            .as_deref(),
        Some(first)
    );
    assert!(!key_rows(&h.store).await.contains(first));

    // Saving the name again replaces the value and its last four.
    let second = "sk_test_51Hxyz987zyx0002";
    let (s, k) = add_key(&h.r, app, "STRIPE_SECRET_KEY", second).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(k["last_four"], "0002");
    let list = list_keys(&h.r, app).await;
    assert_eq!(list.as_array().map(Vec::len), Some(2));
    assert_eq!(list[0]["last_four"], "0002");
    assert_eq!(
        h.vault
            .get(app, "STRIPE_SECRET_KEY")
            .ok()
            .flatten()
            .as_deref(),
        Some(second)
    );
    let rows = key_rows(&h.store).await;
    assert!(!rows.contains(first) && !rows.contains(second));

    // Removing forgets it on the board and in the vault, and is idempotent.
    for _ in 0..2 {
        let (s, _) = call(
            &h.r,
            "POST",
            &format!("/api/apps/{app}/test-keys/STRIPE_SECRET_KEY/remove"),
            Some(OWNER),
            Value::Null,
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT);
    }
    let list = list_keys(&h.r, app).await;
    assert_eq!(list.as_array().map(Vec::len), Some(1));
    assert_eq!(list[0]["name"], "SUPABASE_ANON_KEY");
    assert!(matches!(h.vault.get(app, "STRIPE_SECRET_KEY"), Ok(None)));
}

#[tokio::test]
async fn live_keys_and_reserved_names_are_refused() {
    let h = key_hub().await;
    let app = demo_app(&h.r).await;
    let live = "sk_live_51Habc123def456";
    let (s, msg) = add_key(&h.r, app, "STRIPE_SECRET_KEY", live).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(msg.as_str().is_some_and(|m| m.contains("live")), "{msg}");
    assert!(!msg.to_string().contains(live));
    for name in ["ANTHROPIC_API_KEY", "PATH", "lower_case"] {
        let (s, _) = add_key(&h.r, app, name, "sk_test_51Habc123def456").await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{name}");
    }
    assert_eq!(list_keys(&h.r, app).await, json!([]));
    assert_eq!(key_rows(&h.store).await, "[]");
    assert!(matches!(h.vault.get(app, "STRIPE_SECRET_KEY"), Ok(None)));
    // Nothing was written to the vault at all.
    assert!(!h.dir.exists());
}

#[tokio::test]
async fn a_broken_key_request_never_echoes_the_value() {
    let h = key_hub().await;
    let app = demo_app(&h.r).await;
    let value = "sk_test_51Hsecretvalue77";
    for body in [
        format!(r#"{{"name": "STRIPE_SECRET_KEY", "value": "{value}""#),
        format!(r#"{{"name": 7, "value": "{value}"}}"#),
        format!(r#"{{"value": "{value}"}}"#),
    ] {
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/apps/{app}/test-keys"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {OWNER}"))
            .body(Body::from(body))
            .expect("request");
        let resp = h.r.clone().oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        let text = String::from_utf8_lossy(&bytes);
        assert_eq!(text, "send a name and a value");
    }
}

#[tokio::test]
async fn only_the_owner_manages_test_keys() {
    let h = key_hub().await;
    let app = demo_app(&h.r).await;
    let (s, _) = add_key(&h.r, app, "STRIPE_SECRET_KEY", "sk_test_51Habc123def456").await;
    assert_eq!(s, StatusCode::CREATED);
    let list = format!("/api/apps/{app}/test-keys");
    let remove = format!("/api/apps/{app}/test-keys/STRIPE_SECRET_KEY/remove");
    for (method, path, body) in [
        ("GET", &list, Value::Null),
        (
            "POST",
            &list,
            json!({"name":"OTHER_KEY","value":"sk_test_51Habc123def456"}),
        ),
        ("POST", &remove, Value::Null),
    ] {
        let (s, _) = call(&h.r, method, path, Some(RUNNER), body).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{method} {path}");
    }
    assert_eq!(list_keys(&h.r, app).await.as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn test_keys_of_an_unknown_app_are_not_found() {
    let h = key_hub().await;
    let (s, _) = call(
        &h.r,
        "GET",
        "/api/apps/99/test-keys",
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = add_key(&h.r, 99, "STRIPE_SECRET_KEY", "sk_test_51Habc123def456").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/apps/99/test-keys/STRIPE_SECRET_KEY/remove",
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(!h.dir.exists());
}

#[tokio::test]
async fn a_claim_names_the_apps_test_keys_but_not_their_values() {
    let h = key_hub().await;
    let app = demo_app(&h.r).await;
    for (name, value) in [
        ("SUPABASE_ANON_KEY", "eyJhbGciOiJIUzI1NiJ9.anon9999"),
        ("STRIPE_SECRET_KEY", "sk_test_51Habc123def456"),
    ] {
        let (s, _) = add_key(&h.r, app, name, value).await;
        assert_eq!(s, StatusCode::CREATED);
    }
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/threads",
        Some(OWNER),
        json!({"app_id": app, "title":"t1", "prompt":"do it"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, c) = call(
        &h.r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        c["test_keys"],
        json!(["STRIPE_SECRET_KEY", "SUPABASE_ANON_KEY"])
    );
    let text = c.to_string();
    assert!(!text.contains("sk_test_51Habc123def456") && !text.contains("anon9999"));
}

#[tokio::test]
async fn test_keys_need_a_key_store() {
    let (r, _) = hub().await;
    let app = demo_app(&r).await;
    let (s, _) = call(
        &r,
        "GET",
        &format!("/api/apps/{app}/test-keys"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    let (s, msg) = add_key(&r, app, "STRIPE_SECRET_KEY", "sk_test_51Habc123def456").await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(msg, "the key store is not set up on this hub");
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/apps/{app}/test-keys/STRIPE_SECRET_KEY/remove"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
}

// Owner messages, receipts, task trees and cut-off steps.

async fn mv(r: &axum::Router, id: i64, key: &str, body: Value) -> (StatusCode, Value) {
    call(
        r,
        "POST",
        &format!("/api/threads/{id}/move"),
        Some(key),
        body,
    )
    .await
}

async fn say(r: &axum::Router, id: i64, body: Value) -> (StatusCode, Value) {
    call(
        r,
        "POST",
        &format!("/api/threads/{id}/messages"),
        Some(OWNER),
        body,
    )
    .await
}

async fn event(r: &axum::Router, id: i64, body: Value) -> StatusCode {
    call(
        r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(RUNNER),
        body,
    )
    .await
    .0
}

async fn under(r: &axum::Router, app: i64, parent: i64, background: bool) -> (StatusCode, Value) {
    call(
        r,
        "POST",
        "/api/threads",
        Some(OWNER),
        json!({"app_id": app, "title": "part", "prompt": "p",
               "parent_id": parent, "background": background}),
    )
    .await
}

async fn state(store: &Store, id: i64) -> decurion_proto::ThreadState {
    store.thread(id).await.expect("thread").state
}

async fn last_note(store: &Store, id: i64) -> (String, String, Option<String>) {
    sqlx::query_as("SELECT actor, to_state, note FROM audit WHERE thread_id = ? ORDER BY id DESC")
        .bind(id)
        .fetch_one(store.pool())
        .await
        .expect("audit")
}

async fn hello(r: &axum::Router) {
    let (s, _) = call(
        r,
        "POST",
        "/api/runner/hello",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn a_resent_receipt_gets_the_first_answer() {
    use decurion_proto::ThreadState::*;
    let (r, store) = hub().await;
    let app = demo_app(&r).await;
    let add = json!({"app_id": app, "title":"t", "prompt":"p", "receipt":"add-0000001"});
    let (s, first) = call(&r, "POST", "/api/threads", Some(OWNER), add.clone()).await;
    assert_eq!(s, StatusCode::CREATED);
    let id = first["id"].as_i64().expect("id");
    let (s, again) = call(&r, "POST", "/api/threads", Some(OWNER), add).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(again["id"], id);
    let (_, all) = call(&r, "GET", "/api/threads", Some(OWNER), Value::Null).await;
    assert_eq!(all.as_array().map(Vec::len), Some(1));

    let park = json!({"to":"needs_you", "receipt":"move-000001"});
    let (s, t) = mv(&r, id, OWNER, park.clone()).await;
    assert_eq!(
        (s, t["state"].clone()),
        (StatusCode::OK, json!("needs_you"))
    );
    let audits = store.audit_count(id).await.expect("audit");
    let (s, t) = mv(&r, id, OWNER, park.clone()).await;
    assert_eq!(
        (s, t["state"].clone()),
        (StatusCode::OK, json!("needs_you"))
    );
    assert_eq!(store.audit_count(id).await.expect("audit"), audits);
    // Moved on since: the resend reports the thread as it is now, and does
    // not move it back.
    mv(&r, id, OWNER, json!({"to":"queued"})).await;
    let (s, t) = mv(&r, id, OWNER, park).await;
    assert_eq!((s, t["state"].clone()), (StatusCode::OK, json!("queued")));
    assert_eq!(state(&store, id).await, Queued);

    let note = json!({"kind":"note", "text":"fyi", "receipt":"msg-0000001"});
    let (s, m) = say(&r, id, note.clone()).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, m2) = say(&r, id, note).await;
    assert_eq!((s, m2), (StatusCode::OK, m));
    let (_, all) = call(
        &r,
        "GET",
        &format!("/api/threads/{id}/messages"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(all.as_array().map(Vec::len), Some(1));

    // One tap, one action: a receipt cannot be reused for anything else.
    let (s, _) = say(
        &r,
        id,
        json!({"kind":"note","text":"x","receipt":"add-0000001"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = mv(
        &r,
        id,
        OWNER,
        json!({"to":"failed","receipt":"msg-0000001"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &r,
        "POST",
        "/api/threads",
        Some(OWNER),
        json!({"app_id": app, "title":"t", "prompt":"p", "receipt":"move-000001"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let other = add_work(&r, app, "other").await;
    let (s, _) = mv(
        &r,
        other,
        OWNER,
        json!({"to":"needs_you","receipt":"move-000001"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = say(
        &r,
        other,
        json!({"kind":"note","text":"x","receipt":"msg-0000001"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(state(&store, id).await, Queued);
    assert_eq!(state(&store, other).await, Queued);

    // A malformed receipt is refused rather than ignored.
    let (s, _) = call(
        &r,
        "POST",
        "/api/threads",
        Some(OWNER),
        json!({"app_id": app, "title":"t", "prompt":"p", "receipt":"short"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = mv(
        &r,
        id,
        OWNER,
        json!({"to":"failed","receipt":"has spaces in it"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = say(&r, id, json!({"kind":"note","text":"x","receipt":"short"})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn messages_are_logged_and_steering_sends_work_back() {
    use decurion_proto::ThreadState::*;
    let (r, store) = hub().await;
    let id = seed(&r).await;
    claim(&r).await;
    mv(&r, id, RUNNER, json!({"to":"in_review"})).await;

    // A note is only information: the work stays where it is.
    let (s, m) = say(&r, id, json!({"kind":"note","text":"  the API changed  "})).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(m["text"], "the API changed");
    assert_eq!(m["kind"], "note");
    assert_eq!(m["delivered_at"], Value::Null);
    assert_eq!(state(&store, id).await, InReview);
    let (s, _) = say(&r, id, json!({"kind":"follow_up","text":"also add tests"})).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(state(&store, id).await, Queued);
    assert_eq!(
        last_note(&store, id).await,
        (
            "owner".into(),
            "queued".into(),
            Some("sent back with a message".into())
        )
    );

    let (_, ev) = call(
        &r,
        "GET",
        &format!("/api/threads/{id}/events"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    let owner: Vec<_> = ev
        .as_array()
        .expect("events")
        .iter()
        .filter(|e| e["event"]["kind"] == "owner")
        .map(|e| (e["event"]["via"].clone(), e["event"]["text"].clone()))
        .collect();
    assert_eq!(
        owner,
        vec![
            (json!("note"), json!("the API changed")),
            (json!("follow_up"), json!("also add tests"))
        ]
    );

    // From Needs you and from Failed too.
    let app = store.thread(id).await.expect("thread").app_id;
    let waiting = add_work(&r, app, "waiting").await;
    mv(&r, waiting, OWNER, json!({"to":"needs_you"})).await;
    say(&r, waiting, json!({"kind":"note","text":"hm"})).await;
    assert_eq!(state(&store, waiting).await, NeedsYou);
    say(
        &r,
        waiting,
        json!({"kind":"steer","text":"use the other key"}),
    )
    .await;
    assert_eq!(state(&store, waiting).await, Queued);
    let stopped = add_work(&r, app, "stopped").await;
    mv(&r, stopped, OWNER, json!({"to":"failed"})).await;
    say(&r, stopped, json!({"kind":"note","text":"hm"})).await;
    assert_eq!(state(&store, stopped).await, Failed);
    say(&r, stopped, json!({"kind":"steer","text":"try again"})).await;
    assert_eq!(state(&store, stopped).await, Queued);

    let (s, all) = call(
        &r,
        "GET",
        &format!("/api/threads/{id}/messages"),
        Some(RUNNER),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let kinds: Vec<_> = all
        .as_array()
        .expect("messages")
        .iter()
        .map(|m| m["kind"].clone())
        .collect();
    assert_eq!(kinds, vec![json!("note"), json!("follow_up")]);

    // Length is checked after trimming; only the owner sends.
    let (s, _) = say(&r, id, json!({"kind":"note","text":"   "})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = say(&r, id, json!({"kind":"note","text":"é".repeat(4001)})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = say(&r, id, json!({"kind":"note","text":"é".repeat(4000)})).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = say(&r, id, json!({"kind":"shout","text":"x"})).await;
    assert!(s.is_client_error());
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/threads/{id}/messages"),
        Some(RUNNER),
        json!({"kind":"note","text":"x"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = say(&r, 999, json!({"kind":"note","text":"x"})).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn finished_work_takes_no_messages() {
    let (r, _) = hub().await;
    let id = seed(&r).await;
    claim(&r).await;
    mv(&r, id, RUNNER, json!({"to":"in_review"})).await;
    mv(&r, id, OWNER, json!({"to":"done"})).await;
    let (s, body) = say(&r, id, json!({"kind":"follow_up","text":"one more thing"})).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(body, "this work is finished; add new work instead");
}

#[tokio::test]
async fn the_runner_checks_on_runs_and_acks_messages() {
    let (r, _) = hub().await;
    let id = seed(&r).await;
    let app = demo_app_id(&r).await;
    let other = add_work(&r, app, "other").await;
    claim(&r).await;
    let (_, a) = say(&r, id, json!({"kind":"note","text":"one"})).await;
    let (_, b) = say(&r, id, json!({"kind":"steer","text":"two"})).await;
    let (_, c) = say(&r, other, json!({"kind":"note","text":"elsewhere"})).await;

    let live = format!("/api/threads/{id}/live");
    let delivered = format!("/api/threads/{id}/messages/delivered");
    let (s, _) = call(&r, "GET", &live, Some(OWNER), Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &r,
        "POST",
        &delivered,
        Some(OWNER),
        json!({"ids":[a["id"]]}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let (s, l) = call(&r, "GET", &live, Some(RUNNER), Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(l["state"], "running");
    assert_eq!(l["messages"], json!([a, b]));

    // Another thread's message is not this thread's to ack.
    let ids = json!({"ids":[a["id"], c["id"]]});
    let (s, _) = call(&r, "POST", &delivered, Some(RUNNER), ids.clone()).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, l) = call(&r, "GET", &live, Some(RUNNER), Value::Null).await;
    assert_eq!(l["messages"], json!([b]));
    let (_, all) = call(
        &r,
        "GET",
        &format!("/api/threads/{id}/messages"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    let when = all[0]["delivered_at"].clone();
    assert!(when.is_string());
    let (_, elsewhere) = call(
        &r,
        "GET",
        &format!("/api/threads/{other}/messages"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(elsewhere[0]["delivered_at"], Value::Null);

    // Acking twice changes nothing.
    let (s, _) = call(&r, "POST", &delivered, Some(RUNNER), ids).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, all) = call(
        &r,
        "GET",
        &format!("/api/threads/{id}/messages"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(all[0]["delivered_at"], when);

    // The owner stopping the run shows up for the runner.
    mv(&r, id, OWNER, json!({"to":"failed"})).await;
    let (_, l) = call(&r, "GET", &live, Some(RUNNER), Value::Null).await;
    assert_eq!(l["state"], "failed");
}

async fn demo_app_id(r: &axum::Router) -> i64 {
    let (_, apps) = call(r, "GET", "/api/apps", Some(OWNER), Value::Null).await;
    apps[0]["id"].as_i64().expect("app")
}

#[tokio::test]
async fn a_claim_brings_the_messages_still_waiting() {
    let (r, _) = hub().await;
    let id = seed(&r).await;
    let (_, a) = say(&r, id, json!({"kind":"note","text":"one"})).await;
    let (_, b) = say(&r, id, json!({"kind":"follow_up","text":"two"})).await;
    let c = claim(&r).await;
    assert_eq!(c["messages"], json!([a, b]));
    assert_eq!(c["interrupted"], Value::Null);
    // Handing them out is not delivering them: the runner acks that.
    let (_, l) = call(
        &r,
        "GET",
        &format!("/api/threads/{id}/live"),
        Some(RUNNER),
        Value::Null,
    )
    .await;
    assert_eq!(l["messages"].as_array().map(Vec::len), Some(2));
}

#[tokio::test]
async fn finishing_with_a_follow_up_waiting_goes_back_to_the_queue() {
    use decurion_proto::ThreadState::*;
    let (r, store) = hub().await;
    let id = seed(&r).await;
    claim(&r).await;
    let (_, m) = say(&r, id, json!({"kind":"follow_up","text":"then the docs"})).await;
    assert_eq!(state(&store, id).await, Running);
    let (s, t) = mv(&r, id, RUNNER, json!({"to":"in_review","note":"done"})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t["state"], "queued");
    assert_eq!(
        last_note(&store, id).await,
        (
            "runner:local".into(),
            "queued".into(),
            Some("the owner sent more while it was finishing".into())
        )
    );
    let c = claim(&r).await;
    assert_eq!(c["messages"], json!([m]));
    call(
        &r,
        "POST",
        &format!("/api/threads/{id}/messages/delivered"),
        Some(RUNNER),
        json!({"ids":[m["id"]]}),
    )
    .await;
    // A note never holds work back.
    say(&r, id, json!({"kind":"note","text":"fyi"})).await;
    let (_, t) = mv(&r, id, RUNNER, json!({"to":"in_review"})).await;
    assert_eq!(t["state"], "in_review");
}

#[tokio::test]
async fn stopping_a_task_stops_the_work_under_it_deepest_first() {
    use decurion_proto::ThreadState::*;
    let (r, store) = hub().await;
    let app = demo_app(&r).await;
    let top = add_work(&r, app, "top").await;
    // Out of the queue, so the runner picks up the parts below.
    mv(&r, top, OWNER, json!({"to":"needs_you"})).await;
    let (_, finished) = under(&r, app, top, false).await;
    let finished = finished["id"].as_i64().expect("id");
    claim(&r).await;
    mv(&r, finished, RUNNER, json!({"to":"in_review"})).await;
    mv(&r, finished, OWNER, json!({"to":"done"})).await;
    let (s, child) = under(&r, app, top, false).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(child["parent_id"], top);
    assert_eq!(child["background"], false);
    let child = child["id"].as_i64().expect("id");
    let (_, grandchild) = under(&r, app, child, false).await;
    let grandchild = grandchild["id"].as_i64().expect("id");
    let (_, bg) = under(&r, app, top, true).await;
    assert_eq!(bg["background"], true);
    let bg = bg["id"].as_i64().expect("id");
    let (_, under_bg) = under(&r, app, bg, false).await;
    let under_bg = under_bg["id"].as_i64().expect("id");
    let c = claim(&r).await;
    assert_eq!(c["thread"]["id"], child);
    assert_eq!(c["thread"]["parent_id"], top);

    let (s, t) = mv(&r, top, OWNER, json!({"to":"failed"})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t["state"], "failed");
    assert_eq!(state(&store, child).await, Failed);
    assert_eq!(state(&store, grandchild).await, Failed);
    assert_eq!(state(&store, finished).await, Done);
    assert_eq!(state(&store, bg).await, Queued);
    assert_eq!(state(&store, under_bg).await, Queued);
    let order: Vec<(i64, String, Option<String>)> = sqlx::query_as(
        "SELECT thread_id, actor, note FROM audit WHERE to_state = 'failed' ORDER BY id",
    )
    .fetch_all(store.pool())
    .await
    .expect("audit");
    let why = Some("stopped because the task it is part of was stopped".to_owned());
    assert_eq!(
        order,
        vec![
            (grandchild, "owner".into(), why.clone()),
            (child, "owner".into(), why),
            (top, "owner".into(), None),
        ]
    );
    // The running part's run was closed, not left holding money.
    let open: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE thread_id = ? AND finished_at IS NULL")
            .bind(child)
            .fetch_one(store.pool())
            .await
            .expect("runs");
    assert_eq!(open, 0);
    let (_, l) = call(
        &r,
        "GET",
        &format!("/api/threads/{child}/live"),
        Some(RUNNER),
        Value::Null,
    )
    .await;
    assert_eq!(l["state"], "failed");

    // The runner reporting a failure does not stop the work under it.
    let top2 = add_work(&r, app, "top2").await;
    let (_, part) = under(&r, app, top2, false).await;
    let part = part["id"].as_i64().expect("id");
    mv(&r, bg, OWNER, json!({"to":"needs_you"})).await;
    mv(&r, under_bg, OWNER, json!({"to":"needs_you"})).await;
    let c = claim(&r).await;
    assert_eq!(c["thread"]["id"], top2);
    mv(&r, top2, RUNNER, json!({"to":"failed"})).await;
    assert_eq!(state(&store, part).await, Queued);
}

#[tokio::test]
async fn work_under_a_task_must_fit_the_task() {
    let (r, _) = hub().await;
    let app = demo_app(&r).await;
    let (_, other_app) = call(
        &r,
        "POST",
        "/api/apps",
        Some(OWNER),
        json!({"name":"other","repo_path":"C:\\repos\\other","profile":"B"}),
    )
    .await;
    let top = add_work(&r, app, "top").await;
    let (s, _) = under(&r, app, 999, false).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = under(&r, other_app["id"].as_i64().expect("id"), top, false).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Five levels below the top is as deep as it goes.
    let mut parent = top;
    for _ in 0..5 {
        let (s, t) = under(&r, app, parent, false).await;
        assert_eq!(s, StatusCode::CREATED);
        parent = t["id"].as_i64().expect("id");
    }
    let (s, body) = under(&r, app, parent, false).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(body, "work can only be split 5 levels deep");

    let (_, all) = call(&r, "GET", "/api/threads", Some(OWNER), Value::Null).await;
    assert_eq!(all[0]["parent_id"], Value::Null);
    assert_eq!(all[0]["background"], false);
    assert_eq!(all[1]["parent_id"], top);

    mv(&r, top, OWNER, json!({"to":"failed"})).await;
    let (s, body) = under(&r, app, top, false).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(body, "the task this is part of is finished");
}

#[tokio::test]
async fn a_resumed_run_hears_which_step_was_cut_off() {
    let (r, store) = hub().await;
    let id = seed(&r).await;
    claim(&r).await;
    event(&r, id, json!({"kind":"started"})).await;
    event(
        &r,
        id,
        json!({"kind":"tool","name":"Bash","summary":"cargo test"}),
    )
    .await;
    hello(&r).await;
    // The owner writing in while it is down does not hide the cut-off step.
    say(&r, id, json!({"kind":"note","text":"while you were out"})).await;
    let c = claim(&r).await;
    assert_eq!(c["resume"], true);
    let step = json!({"name":"Bash","summary":"cargo test","safe_to_redo":false});
    assert_eq!(c["interrupted"], step);
    // Cut off again after the box started but before the agent did
    // anything: same step, logged once.
    event(&r, id, json!({"kind":"started"})).await;
    hello(&r).await;
    let c = claim(&r).await;
    assert_eq!(c["interrupted"], step);
    let logged: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM events WHERE thread_id = ?
             AND json_extract(body, '$.kind') = 'interrupted'",
    )
    .bind(id)
    .fetch_one(store.pool())
    .await
    .expect("events");
    assert_eq!(logged, 1);
    let (_, tail) = call(
        &r,
        "GET",
        &format!("/api/threads/{id}/events?tail=2"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    // The cut-off step, then the box that started and died again.
    assert_eq!(tail[1]["event"]["kind"], "started");
    assert_eq!(tail[0]["event"]["kind"], "interrupted");
    assert_eq!(tail[0]["event"]["name"], "Bash");

    // A step the agent got an answer to and talked about is not cut off.
    let app = store.thread(id).await.expect("thread").app_id;
    mv(&r, id, RUNNER, json!({"to":"failed"})).await;
    let next = add_work(&r, app, "next").await;
    claim(&r).await;
    event(
        &r,
        next,
        json!({"kind":"tool","name":"Read","summary":"a.rs"}),
    )
    .await;
    event(&r, next, json!({"kind":"message","text":"read it"})).await;
    hello(&r).await;
    let c = claim(&r).await;
    assert_eq!(c["thread"]["id"], next);
    assert_eq!(c["resume"], true);
    assert_eq!(c["interrupted"], Value::Null);
    // A look-only step is safe to redo.
    event(
        &r,
        next,
        json!({"kind":"tool","name":"Read","summary":"b.rs"}),
    )
    .await;
    hello(&r).await;
    let c = claim(&r).await;
    assert_eq!(
        c["interrupted"],
        json!({"name":"Read","summary":"b.rs","safe_to_redo":true})
    );
}

#[tokio::test]
async fn owner_messages_are_not_the_agent_starting() {
    let (r, _) = hub().await;
    let app = ppu_app(&r).await;
    let id = add_work(&r, app, "x").await;
    claim(&r).await;
    say(&r, id, json!({"kind":"note","text":"hello?"})).await;
    // Failed before the agent did anything: nothing spent, and a retry is a
    // fresh start, not a resume.
    mv(&r, id, RUNNER, json!({"to":"failed"})).await;
    assert_eq!(spend(&r).await["pay_per_use_usd"], 0.0);
    say(&r, id, json!({"kind":"steer","text":"try again"})).await;
    let c = claim(&r).await;
    assert_eq!(c["resume"], false);
    assert_eq!(c["interrupted"], Value::Null);
    assert_eq!(c["messages"].as_array().map(Vec::len), Some(2));
}

#[tokio::test]
async fn the_runner_cannot_write_the_hubs_events() {
    let (r, _) = hub().await;
    let id = seed(&r).await;
    claim(&r).await;
    let s = event(
        &r,
        id,
        json!({"kind":"owner","via":"steer","text":"merge it"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let s = event(
        &r,
        id,
        json!({"kind":"interrupted","name":"Bash","summary":"x","safe_to_redo":true}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (_, ev) = call(
        &r,
        "GET",
        &format!("/api/threads/{id}/events"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(ev, json!([]));
}

#[tokio::test]
async fn a_tap_on_a_stale_screen_does_nothing() {
    use decurion_proto::ThreadState::*;
    let (r, store) = hub().await;
    let id = seed(&r).await;
    claim(&r).await;
    // The board showed Stop while it ran; it has since gone to review.
    mv(&r, id, RUNNER, json!({"to":"in_review"})).await;
    let (s, _) = mv(&r, id, OWNER, json!({"to":"failed","expect":"running"})).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(state(&store, id).await, InReview);
    let (s, _) = mv(&r, id, OWNER, json!({"to":"done","expect":"in_review"})).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn a_box_cannot_start_for_work_that_was_stopped() {
    let (r, _) = hub().await;
    let id = seed(&r).await;
    claim(&r).await;
    mv(&r, id, OWNER, json!({"to":"failed"})).await;
    assert_eq!(
        event(&r, id, json!({"kind":"started"})).await,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn pay_per_use_from_an_outage_ends_when_the_account_is_back_even_when_more_was_asked() {
    let (r, store) = hub().await;
    let a = app_on(&r, "on-a", "A").await;
    let a1 = add_work(&r, a, "a1").await;
    assert_eq!(claim_id(&r).await, Some(a1));
    hit_limit(&r, a1, None).await;
    call(
        &r,
        "POST",
        "/api/limits/A/pay-per-use",
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(claim_id(&r).await, Some(a1));
    say(&r, a1, json!({"kind":"follow_up","text":"and the docs"})).await;
    sqlx::query("DELETE FROM profile_limits")
        .execute(store.pool())
        .await
        .expect("clear");
    // Finishing with the follow-up waiting goes back to the queue, on Max.
    let (_, t) = mv(&r, a1, RUNNER, json!({"to":"in_review"})).await;
    assert_eq!(t["state"], "queued");
    let c = claim(&r).await;
    assert_eq!(c["thread"]["id"], a1);
    assert!(c["budget_usd"].is_null(), "back on the Max account");
}

async fn app_with(r: &axum::Router, name: &str, pay_per_use: bool) -> i64 {
    let (s, app) = call(
        r,
        "POST",
        "/api/apps",
        Some(OWNER),
        json!({"name": name, "repo_path": "C:\\repos\\x", "profile": "B", "pay_per_use": pay_per_use}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    app["id"].as_i64().expect("app id")
}

async fn work(r: &axum::Router, app: i64, ai: &str) -> i64 {
    let (s, t) = call(
        r,
        "POST",
        "/api/threads",
        Some(OWNER),
        json!({"app_id": app, "title": "t", "prompt": "p", "ai": ai}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{t}");
    assert_eq!(t["ai"], ai);
    t["id"].as_i64().expect("thread id")
}

async fn claim_one(r: &axum::Router) -> (StatusCode, Value) {
    call(
        r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await
}

#[tokio::test]
async fn each_ai_runs_on_its_own_account() {
    let (r, _) = hub().await;
    let app = app_with(&r, "mix", false).await;
    work(&r, app, "claude").await;
    work(&r, app, "grok").await;
    work(&r, app, "chatgpt").await;
    let mut seen = vec![];
    for _ in 0..3 {
        let (s, c) = claim_one(&r).await;
        assert_eq!(s, StatusCode::OK);
        seen.push((c["account"].clone(), c["budget_usd"].is_number()));
    }
    assert_eq!(
        seen,
        vec![
            (json!("B"), false),
            (json!("xai"), true),
            (json!("openai"), true)
        ]
    );
    let (s, _) = call(
        &r,
        "POST",
        "/api/threads",
        Some(OWNER),
        json!({"app_id": app, "title": "t", "prompt": "p", "ai": "gemini"}),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_resumed_pi_run_is_budgeted_and_charged_on_its_own() {
    let (r, _) = hub().await;
    let app = app_with(&r, "gpt", false).await;
    let id = work(&r, app, "chatgpt").await;
    let (_, first) = claim_one(&r).await;
    assert_eq!(first["budget_usd"], 9.0);
    for body in [
        json!({"kind":"message","text":"working"}),
        json!({"kind":"finished","ok":false,"summary":"stopped","cost_usd":4.0}),
    ] {
        call(
            &r,
            "POST",
            &format!("/api/threads/{id}/events"),
            Some(RUNNER),
            body,
        )
        .await;
    }
    for (key, to) in [(RUNNER, "needs_you"), (OWNER, "queued")] {
        call(
            &r,
            "POST",
            &format!("/api/threads/{id}/move"),
            Some(key),
            json!({ "to": to }),
        )
        .await;
    }
    let (_, again) = claim_one(&r).await;
    assert_eq!(again["resume"], true);
    // pi reports only this run, so the limit leaves out the $4 already spent.
    assert_eq!(again["budget_usd"], 9.0);
    // It reports $2.50 for this run; the month shows both runs.
    finish(&r, id, Some(2.5)).await;
    let (_, acc) = call(&r, "GET", "/api/accounts", Some(OWNER), json!(null)).await;
    assert_eq!(acc[3]["account"], "openai");
    assert_eq!(acc[3]["spent_usd"], 6.5);
}

#[tokio::test]
async fn every_paid_account_has_its_own_cap() {
    let (r, _) = hub().await;
    let (_, acc) = call(&r, "GET", "/api/accounts", Some(OWNER), json!(null)).await;
    let labels: Vec<&str> = acc
        .as_array()
        .expect("accounts")
        .iter()
        .filter_map(|a| a["label"].as_str())
        .collect();
    assert_eq!(
        labels,
        [
            "Claude A",
            "Claude B",
            "Claude pay-per-use",
            "ChatGPT",
            "Grok"
        ]
    );
    assert_eq!(acc[2]["cap_usd"], 100.0);
    assert_eq!(acc[4]["cap_usd"], 25.0);
    assert!(acc[0]["cap_usd"].is_null());

    let (s, _) = call(
        &r,
        "POST",
        "/api/spend/cap",
        Some(OWNER),
        json!({"account":"xai","cap_usd":0.0}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(
        &r,
        "POST",
        "/api/spend/cap",
        Some(OWNER),
        json!({"account":"A","cap_usd":5.0}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Grok is out of money; Claude pay-per-use is not, and is not held up.
    let app = app_with(&r, "paid", true).await;
    let grok = work(&r, app, "grok").await;
    work(&r, app, "claude").await;
    let (s, c) = claim_one(&r).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(c["account"], "api");
    let (_, t) = call(
        &r,
        "GET",
        &format!("/api/threads/{grok}"),
        Some(OWNER),
        json!(null),
    )
    .await;
    assert_eq!(t["state"], "needs_you");
}

#[tokio::test]
async fn an_app_cap_limits_that_apps_paid_work_only() {
    let (r, _) = hub().await;
    let capped = app_with(&r, "capped", true).await;
    let other = app_with(&r, "other", true).await;
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/apps/{capped}/cap"),
        Some(OWNER),
        json!({"cap_usd": 4.0}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, apps) = call(&r, "GET", "/api/apps", Some(OWNER), json!(null)).await;
    assert_eq!(apps[0]["monthly_cap_usd"], 4.0);

    // The first run gets only what the app has left, not the usual $10.
    work(&r, capped, "claude").await;
    let (_, c) = claim_one(&r).await;
    assert_eq!(c["account"], "api");
    assert!((c["budget_usd"].as_f64().unwrap_or(0.0) - 3.6).abs() < 1e-9);

    // Nothing left for that app; the other app still runs.
    let second = work(&r, capped, "grok").await;
    work(&r, other, "claude").await;
    let (_, c) = claim_one(&r).await;
    assert_eq!(c["app"]["name"], "other");
    assert_eq!(c["budget_usd"], 9.0);
    // The capped app's turn: its work goes to the owner and nothing runs.
    let (s, _) = claim_one(&r).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, t) = call(
        &r,
        "GET",
        &format!("/api/threads/{second}"),
        Some(OWNER),
        json!(null),
    )
    .await;
    assert_eq!(t["state"], "needs_you");

    let (_, sp) = call(&r, "GET", "/api/spend", Some(OWNER), json!(null)).await;
    let capped_row = sp["apps"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["app_id"] == capped))
        .cloned()
        .unwrap_or_default();
    assert_eq!(capped_row["spent_usd"], 4.0);
    assert_eq!(capped_row["cap_usd"], 4.0);

    // Removing the cap lets it run again.
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/apps/{capped}/cap"),
        Some(OWNER),
        json!({"cap_usd": null}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/apps/{capped}/cap"),
        Some(RUNNER),
        json!({"cap_usd": 1.0}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

async fn usage(r: &axum::Router, id: i64, reading: Value) -> StatusCode {
    let mut body = reading;
    body["kind"] = json!("usage");
    call(
        r,
        "POST",
        &format!("/api/threads/{id}/events"),
        Some(RUNNER),
        body,
    )
    .await
    .0
}

async fn accounts(r: &axum::Router) -> Value {
    call(r, "GET", "/api/accounts", Some(OWNER), Value::Null)
        .await
        .1
}

#[tokio::test]
async fn max_plan_usage_fills_its_meter_without_cluttering_the_log() {
    let (r, _) = hub().await;
    let id = seed(&r).await;
    let (_, c) = claim_one(&r).await;
    assert_eq!(c["account"], "A");
    let before = accounts(&r).await;
    assert!(before[0]["seen_at"].is_null() && before[0]["week_pct"].is_null());

    let hour = unix_now() + 3600;
    let week = unix_now() + 3 * 86400;
    let s = usage(
        &r,
        id,
        json!({"window":"five_hour","used_pct":31.0,"resets_at":hour,"status":"allowed"}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    usage(
        &r,
        id,
        json!({"window":"seven_day","used_pct":42.0,"resets_at":week}),
    )
    .await;
    let acc = accounts(&r).await;
    let a = &acc[0];
    assert_eq!(a["account"], "A");
    assert_eq!(a["week_pct"], 42.0);
    assert_eq!(a["five_hour_pct"], 31.0);
    assert_eq!(a["limited"], false);
    assert!(a["seen_at"].is_string());
    // The fullest allowance is the week, so its reset is the one shown.
    let shown = a["resets_at"].as_str().unwrap_or_default().to_owned();
    assert!(shown.ends_with('Z'), "{shown}");
    assert_eq!(a["week_resets_at"], json!(shown));
    assert!(a["five_hour_resets_at"]
        .as_str()
        .is_some_and(|t| t.ends_with('Z') && t != shown));
    let (_, ev) = call(
        &r,
        "GET",
        &format!("/api/threads/{id}/events"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(ev, json!([]));
    // Plan B was not used and has nothing to show.
    assert!(acc[1]["seen_at"].is_null() && acc[1]["week_pct"].is_null());

    // The plan runs out for the week; the meter says so but the work is left
    // where it is.
    usage(
        &r,
        id,
        json!({"window":"seven_day","used_pct":100.0,"resets_at":week,"status":"rejected"}),
    )
    .await;
    let a = &accounts(&r).await[0];
    assert_eq!(a["limited"], true);
    assert_eq!(a["week_pct"], 100.0);
    assert_eq!(a["resets_at"], json!(shown));
    let (_, t) = call(
        &r,
        "GET",
        &format!("/api/threads/{id}"),
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(t["state"], "running");
    // A later reading says it is fine again.
    usage(&r, id, json!({"window":"five_hour","status":"allowed"})).await;
    let a = &accounts(&r).await[0];
    assert_eq!(a["limited"], false);
    assert_eq!(
        a["five_hour_pct"], 31.0,
        "a reading without a figure keeps the last"
    );
}

#[tokio::test]
async fn usage_counts_only_for_a_running_max_plan_run() {
    let (r, _) = hub().await;
    let week = unix_now() + 86400;
    let paid = ppu_app(&r).await;
    let id = add_work(&r, paid, "paid").await;
    let (_, c) = claim_one(&r).await;
    assert_eq!(c["account"], "api");
    let s = usage(
        &r,
        id,
        json!({"window":"seven_day","used_pct":50.0,"resets_at":week}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let acc = accounts(&r).await;
    assert!(acc[0]["seen_at"].is_null() && acc[1]["seen_at"].is_null());

    // Without a run going, a reading has no plan to go to.
    finish(&r, id, None).await;
    let max = seed(&r).await;
    usage(&r, max, json!({"window":"seven_day","used_pct":50.0})).await;
    assert!(accounts(&r).await[0]["seen_at"].is_null());

    // Only the runner reports usage, and odd values are tamed.
    claim_one(&r).await;
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/threads/{max}/events"),
        Some(OWNER),
        json!({"kind":"usage","window":"seven_day","used_pct":5.0}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    usage(&r, max, json!({"window":"seven_day","used_pct":250.0})).await;
    usage(&r, max, json!({"window":"Seven Day!","used_pct":1.0})).await;
    assert_eq!(accounts(&r).await[0]["week_pct"], 100.0);

    // A week that has already renewed starts again from zero, and an old
    // "out" no longer holds.
    usage(
        &r,
        max,
        json!({"window":"seven_day","used_pct":100.0,"resets_at":1_700_000_000,"status":"rejected"}),
    )
    .await;
    let a = &accounts(&r).await[0];
    assert_eq!(a["week_pct"], 0.0);
    assert_eq!(a["limited"], false);
}

#[tokio::test]
async fn chatgpt_and_grok_work_does_not_wait_on_a_max_outage() {
    let (r, _) = hub().await;
    let a = app_on(&r, "on-a", "A").await;
    let a1 = add_work(&r, a, "a1").await;
    assert_eq!(claim_id(&r).await, Some(a1));
    hit_limit(&r, a1, None).await;
    let grok = work(&r, a, "grok").await;
    // Only the Claude work counts as waiting on the account.
    let (_, limits) = call(&r, "GET", "/api/limits", Some(OWNER), Value::Null).await;
    assert_eq!(limits[0]["waiting"], 1);
    let (_, c) = claim_one(&r).await;
    assert_eq!(c["thread"]["id"], grok);
    assert_eq!(c["account"], "xai");
    // The one tap moves the Claude work, which then runs on pay-per-use.
    let (_, moved) = call(
        &r,
        "POST",
        "/api/limits/A/pay-per-use",
        Some(OWNER),
        Value::Null,
    )
    .await;
    assert_eq!(moved["waiting"], 0);
    let (_, c) = claim_one(&r).await;
    assert_eq!(c["thread"]["id"], a1);
    assert_eq!(c["account"], "api");
    assert!(c["budget_usd"].is_number());
}

#[tokio::test]
async fn forged_usage_windows_cannot_grow_the_table() {
    let (r, store) = hub().await;
    let id = seed(&r).await;
    claim_one(&r).await;
    for i in 0..30 {
        usage(
            &r,
            id,
            json!({"window": format!("seven_day_x{i}"), "used_pct": 1.0}),
        )
        .await;
    }
    // A real window already kept still updates.
    usage(&r, id, json!({"window":"seven_day_x0","used_pct":55.0})).await;
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM account_usage")
        .fetch_one(store.pool())
        .await
        .unwrap_or(-1);
    assert_eq!(n, 12);
    assert_eq!(accounts(&r).await[0]["week_pct"], 55.0);
}

// ---------- Apps take turns, go first today, and can be changed ----------

async fn star(r: &axum::Router, app: i64, body: Value) -> (StatusCode, Value) {
    call(
        r,
        "POST",
        &format!("/api/apps/{app}/first"),
        Some(OWNER),
        body,
    )
    .await
}

async fn edit(r: &axum::Router, app: i64, body: Value) -> (StatusCode, Value) {
    call(
        r,
        "POST",
        &format!("/api/apps/{app}/edit"),
        Some(OWNER),
        body,
    )
    .await
}

#[tokio::test]
async fn apps_take_turns_in_the_queue() {
    let (r, _) = hub().await;
    let a = app_on(&r, "alpha", "A").await;
    let b = app_on(&r, "beta", "A").await;
    let a1 = add_work(&r, a, "a1").await;
    let a2 = add_work(&r, a, "a2").await;
    let a3 = add_work(&r, a, "a3").await;
    let b1 = add_work(&r, b, "b1").await;
    // First come, first served would hand out a1, a2, a3 before b1.
    for want in [a1, b1, a2, a3] {
        assert_eq!(claim_id(&r).await, Some(want));
    }
    assert_eq!(claim_id(&r).await, None);
}

#[tokio::test]
async fn a_starred_app_goes_first_today() {
    let (r, _) = hub().await;
    let a = app_on(&r, "alpha", "A").await;
    let b = app_on(&r, "beta", "A").await;
    let a1 = add_work(&r, a, "a1").await;
    let a2 = add_work(&r, a, "a2").await;
    let b1 = add_work(&r, b, "b1").await;
    let b2 = add_work(&r, b, "b2").await;
    let (s, app) = star(&r, b, json!({"on": true})).await;
    assert_eq!(s, StatusCode::OK, "{app}");
    assert_eq!(app["first_today"], true);
    let (_, apps) = call(&r, "GET", "/api/apps", Some(OWNER), Value::Null).await;
    assert_eq!(apps[0]["first_today"], false);
    assert_eq!(apps[1]["first_today"], true);
    // The starred app's work comes before the other app's turn.
    assert_eq!(claim_id(&r).await, Some(b1));
    assert_eq!(claim_id(&r).await, Some(b2));
    assert_eq!(claim_id(&r).await, Some(a1));
    let (s, app) = star(&r, b, json!({"on": false})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(app["first_today"], false);
    assert_eq!(claim_id(&r).await, Some(a2));
}

#[tokio::test]
async fn yesterdays_star_does_not_count() {
    let (r, store) = hub().await;
    let a = app_on(&r, "alpha", "A").await;
    let b = app_on(&r, "beta", "A").await;
    let a1 = add_work(&r, a, "a1").await;
    add_work(&r, b, "b1").await;
    star(&r, b, json!({"on": true})).await;
    sqlx::query("UPDATE apps SET first_on = date('now', 'localtime', '-1 day') WHERE id = ?")
        .bind(b)
        .execute(store.pool())
        .await
        .expect("move the star to yesterday");
    let (_, apps) = call(&r, "GET", "/api/apps", Some(OWNER), Value::Null).await;
    assert_eq!(apps[1]["first_today"], false);
    assert_eq!(claim_id(&r).await, Some(a1));
}

#[tokio::test]
async fn only_the_owner_stars_an_app() {
    let (r, _) = hub().await;
    let a = app_on(&r, "alpha", "A").await;
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/apps/{a}/first"),
        Some(RUNNER),
        json!({"on": true}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (_, apps) = call(&r, "GET", "/api/apps", Some(OWNER), Value::Null).await;
    assert_eq!(apps[0]["first_today"], false);
    let (s, _) = star(&r, 99, json!({"on": true})).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_resent_star_does_nothing_twice() {
    let (r, _) = hub().await;
    let a = app_on(&r, "alpha", "A").await;
    let b = app_on(&r, "beta", "A").await;
    let (_, app) = star(&r, a, json!({"on": true, "receipt": "star-tap-0001"})).await;
    assert_eq!(app["first_today"], true);
    let (_, app) = star(&r, a, json!({"on": false, "receipt": "star-tap-0002"})).await;
    assert_eq!(app["first_today"], false);
    // The first tap arrives again: the star stays off.
    let (s, app) = star(&r, a, json!({"on": true, "receipt": "star-tap-0001"})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(app["first_today"], false);
    // One tap is one action, for one app.
    let (s, _) = star(&r, b, json!({"on": true, "receipt": "star-tap-0001"})).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = edit(&r, a, json!({"name": "gamma", "receipt": "star-tap-0001"})).await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[tokio::test]
async fn the_owner_changes_an_apps_name_folder_plan_and_reviewer() {
    let (r, store) = hub().await;
    let a = app_on(&r, "refill", "A").await;
    let (s, app) = edit(&r, a, json!({"name": "refill-tracker"})).await;
    assert_eq!(s, StatusCode::OK, "{app}");
    assert_eq!(app["name"], "refill-tracker");
    assert_eq!(app["repo_path"], "C:\\repos\\x");
    assert_eq!(app["profile"], "A");
    assert_eq!(app["reviewer"], Value::Null);

    let (_, app) = edit(&r, a, json!({"repo_path": "/home/me/refill"})).await;
    assert_eq!(app["repo_path"], "/home/me/refill");
    assert_eq!(app["name"], "refill-tracker");

    let (_, app) = edit(&r, a, json!({"reviewer": " @Param-K "})).await;
    assert_eq!(app["reviewer"], "Param-K");
    // Left out, the reviewer stays.
    let (_, app) = edit(&r, a, json!({"name": "refill"})).await;
    assert_eq!(app["reviewer"], "Param-K");
    let (_, app) = edit(&r, a, json!({"reviewer": ""})).await;
    assert_eq!(app["reviewer"], Value::Null);
    edit(&r, a, json!({"reviewer": "param-k"})).await;
    let (_, app) = edit(&r, a, json!({"reviewer": null})).await;
    assert_eq!(app["reviewer"], Value::Null);

    // Running work keeps its plan; the next run uses the new one.
    let first = add_work(&r, a, "first").await;
    assert_eq!(claim_id(&r).await, Some(first));
    let (s, app) = edit(&r, a, json!({"profile": "B"})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(app["profile"], "B");
    assert_eq!(
        store.open_run_account(first).await.expect("account"),
        Some("A".into())
    );
    add_work(&r, a, "second").await;
    let (_, c) = claim_one(&r).await;
    assert_eq!(c["account"], "B");
    assert_eq!(c["app"]["profile"], "B");
}

#[tokio::test]
async fn bad_app_changes_are_refused_in_plain_words() {
    let (r, _) = hub().await;
    let a = app_on(&r, "alpha", "A").await;
    for body in [
        json!({"name": "Smith, Inc"}),
        json!({"name": ""}),
        json!({"repo_path": "relative\\path"}),
        json!({"repo_path": "C:\\repos\\a,b"}),
    ] {
        let (s, msg) = edit(&r, a, body.clone()).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
        assert!(msg.as_str().is_some_and(|m| !m.is_empty()), "{body}");
    }
    let long = "x".repeat(40);
    for login in [
        "-param",
        "param-",
        "pa--ram",
        "has space",
        "dot.name",
        &long,
    ] {
        let (s, msg) = edit(&r, a, json!({"reviewer": login})).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{login}");
        assert!(
            msg.as_str()
                .is_some_and(|m| m.contains("reviewer's GitHub name")),
            "{login}: {msg}"
        );
    }
    let (_, app) = edit(&r, a, json!({"reviewer": "x".repeat(39)})).await;
    assert_eq!(app["reviewer"], "x".repeat(39));
    // Nothing changed by the refused ones.
    assert_eq!(app["name"], "alpha");
    assert_eq!(app["repo_path"], "C:\\repos\\x");
    let (s, _) = edit(&r, 99, json!({"name": "ghost"})).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_app_cannot_take_another_apps_name() {
    let (r, _) = hub().await;
    app_on(&r, "alpha", "A").await;
    let b = app_on(&r, "beta", "A").await;
    for name in ["alpha", "ALPHA"] {
        let (s, msg) = edit(&r, b, json!({"name": name})).await;
        assert_eq!(s, StatusCode::CONFLICT, "{name}");
        assert!(
            msg.as_str().is_some_and(|m| m.contains("already called")),
            "{msg}"
        );
    }
    // Its own name, even in other capitals, is fine.
    let (s, app) = edit(&r, b, json!({"name": "Beta"})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(app["name"], "Beta");
}

async fn edit_refused(r: &axum::Router, app: i64, body: Value) -> String {
    let (s, msg) = edit(r, app, body).await;
    assert_eq!(s, StatusCode::CONFLICT, "{msg}");
    msg.as_str().unwrap_or_default().to_owned()
}

#[tokio::test]
async fn an_app_keeps_its_name_and_folder_while_its_work_is_underway() {
    let (r, _) = hub().await;
    let a = app_on(&r, "alpha", "A").await;
    let id = add_work(&r, a, "big job").await;
    // Waiting work that never started does not hold up a rename.
    let (s, _) = edit(&r, a, json!({"name": "alpha-1"})).await;
    assert_eq!(s, StatusCode::OK);

    assert_eq!(claim_id(&r).await, Some(id));
    let msg = edit_refused(&r, a, json!({"name": "alpha-2"})).await;
    assert!(msg.contains("then rename it"), "{msg}");
    let msg = edit_refused(&r, a, json!({"repo_path": "/repos/alpha"})).await;
    assert!(msg.contains("then change its folder"), "{msg}");
    // The plan and the reviewer can change any time.
    let (s, _) = edit(&r, a, json!({"profile": "B", "reviewer": "param-k"})).await;
    assert_eq!(s, StatusCode::OK);
    // The same name and folder are not a change.
    let (s, _) = edit(
        &r,
        a,
        json!({"name": "alpha-1", "repo_path": "C:\\repos\\x"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // Paused partway by a runner restart: it resumes from its folders.
    let said = event(&r, id, json!({"kind":"message","text":"working"})).await;
    assert_eq!(said, StatusCode::CREATED);
    let (s, _) = call(
        &r,
        "POST",
        "/api/runner/hello",
        Some(RUNNER),
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    edit_refused(&r, a, json!({"name": "alpha-2"})).await;

    // Once it is in review its saved copy is on the hub.
    assert_eq!(claim_id(&r).await, Some(id));
    finish(&r, id, None).await;
    let (s, app) = edit(
        &r,
        a,
        json!({"name": "alpha-2", "repo_path": "/repos/alpha"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{app}");
    assert_eq!(app["name"], "alpha-2");
    assert_eq!(app["repo_path"], "/repos/alpha");
}

#[tokio::test]
async fn only_the_owner_changes_an_app() {
    let (r, _) = hub().await;
    let a = app_on(&r, "alpha", "A").await;
    for body in [
        json!({"name": "mine"}),
        json!({"profile": "B"}),
        json!({"reviewer": "param-k"}),
    ] {
        let (s, _) = call(
            &r,
            "POST",
            &format!("/api/apps/{a}/edit"),
            Some(RUNNER),
            body,
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    let (_, apps) = call(&r, "GET", "/api/apps", Some(OWNER), Value::Null).await;
    assert_eq!(apps[0]["name"], "alpha");
    assert_eq!(apps[0]["profile"], "A");
    assert_eq!(apps[0]["reviewer"], Value::Null);
}

#[tokio::test]
async fn a_resent_app_change_does_nothing_twice() {
    let (r, _) = hub().await;
    let a = app_on(&r, "alpha", "A").await;
    let (_, app) = edit(&r, a, json!({"name": "beta", "receipt": "edit-tap-0001"})).await;
    assert_eq!(app["name"], "beta");
    let (_, app) = edit(&r, a, json!({"name": "gamma", "receipt": "edit-tap-0002"})).await;
    assert_eq!(app["name"], "gamma");
    let (s, app) = edit(&r, a, json!({"name": "beta", "receipt": "edit-tap-0001"})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(app["name"], "gamma");
    let (s, _) = edit(&r, a, json!({"name": "delta", "receipt": "bad"})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}
