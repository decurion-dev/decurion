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
        json!({"runner":"local"}),
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
        json!({"runner":"local"}),
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
        json!({"runner":"local"}),
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
        json!({"runner":"local"}),
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
        json!({"runner":"local"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(paused, json!([id]));
    assert_eq!(
        store.thread(id).await.expect("thread").state,
        decurion_proto::ThreadState::Queued
    );

    let (_, again) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"local"}),
    )
    .await;
    assert_eq!(again["resume"], true);
    assert_eq!(again["thread"]["session_id"], session);
    assert_eq!(again["thread"]["branch"], first["thread"]["branch"]);
    // started, paused, resumed
    assert_eq!(store.audit_count(id).await.expect("audit"), 3);
}

#[tokio::test]
async fn events_come_only_from_the_runner() {
    let (r, _) = hub().await;
    let id = seed(&r).await;
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
        json!({"runner":"local"}),
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
        json!({"runner":"local"}),
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
        json!({"runner":"local"}),
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
        json!({"runner":"local"}),
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
        json!({"runner":"local"}),
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
    let (s, _) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"owner"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let actor: String = sqlx::query_scalar("SELECT actor FROM audit WHERE thread_id = ?")
        .bind(id)
        .fetch_one(store.pool())
        .await
        .expect("audit row");
    assert_eq!(actor, "runner:owner");
    let (s, _) = call(
        &r,
        "POST",
        "/api/runner/claim",
        Some(RUNNER),
        json!({"runner":"Owner Key"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
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
