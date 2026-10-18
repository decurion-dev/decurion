//! Watching and taking over the agent's test browser: who may open which
//! end, the notes the agent gets, and the relay itself over real sockets.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::browser::{HANDED_BACK_NOTE, TAKEN_NOTE};
use decurion_hub::{router, store::Store, AppState, Keys};
use decurion_proto::browser::{MAX_FRAME_BYTES, MAX_INPUT_BYTES};
use futures_util::{SinkExt, StreamExt};
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

async fn hub_with(hand_back_after: Duration) -> (axum::Router, Store) {
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
        .with_hand_back_after(hand_back_after),
    );
    (r, store)
}

async fn hub() -> (axum::Router, Store) {
    hub_with(Duration::from_secs(600)).await
}

async fn send(
    r: &axum::Router,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Value,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    for (k, v) in headers {
        b = b.header(*k, *v);
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

async fn call(
    r: &axum::Router,
    method: &str,
    path: &str,
    key: &str,
    body: Value,
) -> (StatusCode, Value) {
    let auth = format!("Bearer {key}");
    send(r, method, path, &[("authorization", &auth)], body).await
}

/// An app and one piece of work by `ai`, queued.
async fn work(r: &axum::Router, ai: &str) -> i64 {
    let (s, app) = call(
        r,
        "POST",
        "/api/apps",
        OWNER,
        json!({"name": format!("demo-{ai}"), "repo_path": "C:\\repos\\demo", "profile": "A"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{app}");
    let (s, t) = call(
        r,
        "POST",
        "/api/threads",
        OWNER,
        json!({"app_id": app["id"], "title": "t", "prompt": "do it", "ai": ai}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{t}");
    t["id"].as_i64().expect("id")
}

/// Claude work running on the PC runner.
async fn running(r: &axum::Router) -> i64 {
    let id = work(r, "claude").await;
    let (s, c) = call(
        r,
        "POST",
        "/api/runner/claim",
        RUNNER,
        json!({"runner": "local", "rules_version": decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["thread"]["id"], id);
    id
}

async fn watch(r: &axum::Router, id: i64) -> i64 {
    let (s, w) = call(
        r,
        "POST",
        &format!("/api/threads/{id}/browser/watch"),
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{w}");
    w["id"].as_i64().expect("session id")
}

async fn notes(r: &axum::Router, id: i64) -> Vec<String> {
    let (s, m) = call(
        r,
        "GET",
        &format!("/api/threads/{id}/messages"),
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    m.as_array()
        .expect("messages")
        .iter()
        .filter(|m| m["kind"] == "note")
        .map(|m| m["text"].as_str().unwrap_or_default().to_owned())
        .collect()
}

async fn live(r: &axum::Router, id: i64) -> Value {
    let (s, l) = call(
        r,
        "GET",
        &format!("/api/threads/{id}/live"),
        RUNNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{l}");
    l
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
async fn watching_needs_the_owner_and_running_claude_work() {
    let (r, _) = hub().await;
    let queued = work(&r, "claude").await;
    let path = format!("/api/threads/{queued}/browser/watch");
    let (s, _) = call(&r, "POST", &path, OWNER, Value::Null).await;
    assert_eq!(s, StatusCode::CONFLICT, "nothing to watch before it runs");

    let other = work(&r, "chatgpt").await;
    let (s, why) = call(
        &r,
        "POST",
        &format!("/api/threads/{other}/browser/watch"),
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(why.as_str().is_some_and(|w| w.contains("Claude")), "{why}");

    // Claiming gives the runner the oldest queued work: the Claude one.
    let (s, c) = call(
        &r,
        "POST",
        "/api/runner/claim",
        RUNNER,
        json!({"runner": "local", "rules_version": decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["thread"]["id"], queued);

    let (s, _) = call(&r, "POST", &path, RUNNER, Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "the runner can't ask to watch");
    let (s, w) = call(&r, "POST", &path, OWNER, Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{w}");
    assert_eq!(w["kind"], "take_over");
    assert_eq!(w["state"], "pending");
    assert_eq!(w["controller"], "agent");
    assert_eq!(w["runner"], "local");
    let (_, again) = call(&r, "POST", &path, OWNER, Value::Null).await;
    assert_eq!(again["id"], w["id"], "an open view is reused");

    let (s, _) = call(
        &r,
        "POST",
        "/api/threads/999/browser/watch",
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_runner_end_is_only_for_the_runner_holding_the_work() {
    let (r, store) = hub().await;
    let id = running(&r).await;
    let sid = watch(&r, id).await;
    let path = format!("/api/runner/browser/{sid}/ws");

    let (s, _) = call(&r, "GET", &path, OWNER, Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "the owner key is not a runner");
    let (_, token) = store.add_device("phone").await.expect("device");
    let cookie = format!("decurion_device={token}");
    let (s, _) = send(&r, "GET", &path, &[("cookie", &cookie)], Value::Null).await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "a signed-in phone is not a runner"
    );
    let cloud = cloud_key(&store).await;
    let (s, _) = call(&r, "GET", &path, &cloud, Value::Null).await;
    assert_eq!(s, StatusCode::CONFLICT, "another runner's browser");
    let (s, _) = call(&r, "GET", "/api/runner/browser/999/ws", RUNNER, Value::Null).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // The runner holding the work gets as far as the WebSocket handshake.
    let (s, _) = call(&r, "GET", &path, RUNNER, Value::Null).await;
    assert!(
        ![
            StatusCode::FORBIDDEN,
            StatusCode::CONFLICT,
            StatusCode::NOT_FOUND
        ]
        .contains(&s),
        "{s}"
    );
}

#[tokio::test]
async fn the_owners_end_refuses_runner_keys_and_other_sites() {
    let (r, store) = hub().await;
    let id = running(&r).await;
    let sid = watch(&r, id).await;
    let path = format!("/api/browser/{sid}/ws");

    let (s, _) = call(&r, "GET", &path, RUNNER, Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let cloud = cloud_key(&store).await;
    let (s, _) = call(&r, "GET", &path, &cloud, Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    for (k, p) in [("take", "take"), ("give back", "give-back")] {
        let (s, _) = call(
            &r,
            "POST",
            &format!("/api/browser/{sid}/{p}"),
            RUNNER,
            Value::Null,
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "the runner can't {k}");
    }

    // The board's cookie works only from the hub's own pages.
    let (_, token) = store.add_device("phone").await.expect("device");
    let cookie = format!("decurion_device={token}");
    let (s, _) = send(
        &r,
        "GET",
        &path,
        &[
            ("cookie", &cookie),
            ("host", "hub.test"),
            ("origin", "http://hub.test:3000"),
        ],
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "another port's page");
    let (s, _) = send(
        &r,
        "GET",
        &path,
        &[("cookie", &cookie), ("sec-fetch-site", "same-origin")],
        Value::Null,
    )
    .await;
    assert!(
        ![StatusCode::FORBIDDEN, StatusCode::UNAUTHORIZED].contains(&s),
        "{s}"
    );
}

#[tokio::test]
async fn taking_over_and_handing_back_tell_the_agent() {
    let (r, _) = hub().await;
    let id = running(&r).await;
    let sid = watch(&r, id).await;
    let (s, t) = call(
        &r,
        "POST",
        &format!("/api/browser/{sid}/take"),
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{t}");
    assert_eq!(t["controller"], "owner");
    // A second tap changes nothing and adds nothing.
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/browser/{sid}/take"),
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(notes(&r, id).await, vec![TAKEN_NOTE.to_owned()]);
    // The runner hands it to the agent like any other note.
    let l = live(&r, id).await;
    assert_eq!(l["messages"][0]["text"], TAKEN_NOTE);
    assert_eq!(l["messages"][0]["kind"], "note");

    let (s, g) = call(
        &r,
        "POST",
        &format!("/api/browser/{sid}/give-back"),
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{g}");
    assert_eq!(g["controller"], "agent");
    assert_eq!(
        notes(&r, id).await,
        vec![TAKEN_NOTE.to_owned(), HANDED_BACK_NOTE.to_owned()]
    );
    // The notes do not stop or move the work.
    let (_, t) = call(&r, "GET", &format!("/api/threads/{id}"), OWNER, Value::Null).await;
    assert_eq!(t["state"], "running");
}

#[tokio::test]
async fn the_owner_hands_back_on_their_own_after_a_quiet_spell() {
    let (r, _) = hub_with(Duration::from_millis(200)).await;
    let id = running(&r).await;
    let sid = watch(&r, id).await;
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/browser/{sid}/take"),
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    for _ in 0..50 {
        if notes(&r, id).await.len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        notes(&r, id).await,
        vec![TAKEN_NOTE.to_owned(), HANDED_BACK_NOTE.to_owned()]
    );
}

// ---------- The relay, over real sockets ----------

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

/// True if nothing but pings arrives for a moment.
async fn quiet(ws: &mut Ws) -> bool {
    loop {
        match tokio::time::timeout(Duration::from_millis(300), ws.next()).await {
            Err(_) => return true,
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
            Ok(_) => return false,
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

async fn put(ws: &mut Ws, v: Value) {
    ws.send(Message::text(v.to_string())).await.expect("send");
}

fn frame(seq: u64, jpeg: String) -> Value {
    json!({"type": "frame", "seq": seq, "jpeg": jpeg, "width": 800, "height": 600})
}

#[tokio::test]
async fn the_relay_passes_frames_down_and_input_up() {
    let (r, store) = hub().await;
    let id = running(&r).await;
    let sid = watch(&r, id).await;
    let addr = serve(r.clone()).await;

    assert_eq!(
        live(&r, id).await["browser"],
        Value::Null,
        "nobody watching"
    );
    let mut viewer = connect(addr, &format!("/api/browser/{sid}/ws?max_width=780"), OWNER).await;
    assert_eq!(
        next(&mut viewer).await,
        json!({"type": "control", "who": "agent"})
    );
    assert_eq!(
        next(&mut viewer).await["type"],
        "ended",
        "waiting for the runner"
    );
    assert_eq!(live(&r, id).await["browser"], json!({"session_id": sid}));

    let mut runner = connect(addr, &format!("/api/runner/browser/{sid}/ws"), RUNNER).await;
    assert_eq!(
        next(&mut runner).await,
        json!({"type": "control", "who": "agent"})
    );
    assert_eq!(
        next(&mut runner).await,
        json!({"type": "watch", "on": true, "max_width": 780})
    );
    assert_eq!(
        store.browser_session(sid).await.expect("session").state,
        "live"
    );

    // The page address and frames go down.
    let page = json!({"type": "page", "url": "http://localhost:5173/login", "title": "Log in"});
    put(&mut runner, page.clone()).await;
    assert_eq!(next(&mut viewer).await, page);
    put(&mut runner, frame(1, "AAAA".into())).await;
    assert_eq!(next(&mut viewer).await, frame(1, "AAAA".into()));
    // One frame in flight: 2 waits, 3 replaces it, and 2 is let go.
    put(&mut runner, frame(2, "BBBB".into())).await;
    put(&mut runner, frame(3, "CCCC".into())).await;
    assert_eq!(
        next(&mut runner).await,
        json!({"type": "frame_ack", "seq": 2})
    );
    assert!(
        quiet(&mut viewer).await,
        "nothing more until the screen is ready"
    );
    put(&mut viewer, json!({"type": "frame_ack", "seq": 1})).await;
    assert_eq!(
        next(&mut runner).await,
        json!({"type": "frame_ack", "seq": 1})
    );
    assert_eq!(next(&mut viewer).await, frame(3, "CCCC".into()));
    put(&mut viewer, json!({"type": "frame_ack", "seq": 3})).await;
    assert_eq!(
        next(&mut runner).await,
        json!({"type": "frame_ack", "seq": 3})
    );

    // Input goes up only while the owner has the browser.
    let click = json!({"type": "input", "input": {"kind": "click", "x": 0.5, "y": 0.25}});
    put(&mut viewer, click.clone()).await;
    assert!(quiet(&mut runner).await, "the agent has the browser");
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/browser/{sid}/take"),
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        next(&mut runner).await,
        json!({"type": "control", "who": "owner"})
    );
    assert_eq!(
        next(&mut viewer).await,
        json!({"type": "control", "who": "owner"})
    );
    put(&mut viewer, click.clone()).await;
    assert_eq!(next(&mut runner).await, click);

    // Input over the size limit is dropped; the next piece still goes.
    let big = "x".repeat(MAX_INPUT_BYTES);
    put(
        &mut viewer,
        json!({"type": "input", "input": {"kind": "text", "text": big}}),
    )
    .await;
    let enter = json!({"type": "input", "input": {"kind": "key", "key": "Enter"}});
    put(&mut viewer, enter.clone()).await;
    assert_eq!(next(&mut runner).await, enter);

    // A frame over the size limit never reaches the screen.
    put(&mut runner, frame(4, "A".repeat(MAX_FRAME_BYTES + 1))).await;
    assert_eq!(
        next(&mut runner).await,
        json!({"type": "frame_ack", "seq": 4})
    );
    assert!(quiet(&mut viewer).await);

    // Closing the view hands the browser back and stops the frames.
    viewer.close(None).await.expect("close");
    assert_eq!(
        next(&mut runner).await,
        json!({"type": "watch", "on": false, "max_width": 780})
    );
    assert_eq!(
        next(&mut runner).await,
        json!({"type": "control", "who": "agent"})
    );
    assert_eq!(
        notes(&r, id).await,
        vec![TAKEN_NOTE.to_owned(), HANDED_BACK_NOTE.to_owned()]
    );
    assert_eq!(live(&r, id).await["browser"], Value::Null);

    // A message far over the limit closes the runner's socket.
    put(&mut runner, frame(5, "A".repeat(MAX_FRAME_BYTES * 2))).await;
    assert!(closed(&mut runner).await);
    for _ in 0..50 {
        if store.browser_session(sid).await.expect("session").state == "pending" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        store.browser_session(sid).await.expect("session").state,
        "pending",
        "waiting for the runner to come back"
    );
}

#[tokio::test]
async fn the_view_ends_when_the_browser_does() {
    let (r, store) = hub().await;
    let id = running(&r).await;
    let sid = watch(&r, id).await;
    let addr = serve(r.clone()).await;
    let mut runner = connect(addr, &format!("/api/runner/browser/{sid}/ws"), RUNNER).await;
    assert_eq!(next(&mut runner).await["type"], "control");
    let mut viewer = connect(addr, &format!("/api/browser/{sid}/ws"), OWNER).await;
    assert_eq!(next(&mut viewer).await["type"], "control");
    assert_eq!(
        next(&mut runner).await,
        json!({"type": "watch", "on": true, "max_width": 1280})
    );

    // A second screen takes the view over from the first.
    let mut second = connect(addr, &format!("/api/browser/{sid}/ws"), OWNER).await;
    assert_eq!(next(&mut second).await["type"], "control");
    assert_eq!(next(&mut viewer).await["type"], "ended");
    assert!(closed(&mut viewer).await);

    put(
        &mut runner,
        json!({"type": "ended", "why": "The run finished."}),
    )
    .await;
    assert_eq!(
        next(&mut second).await,
        json!({"type": "ended", "why": "The run finished."})
    );
    assert!(closed(&mut second).await);
    assert_eq!(
        store.browser_session(sid).await.expect("session").state,
        "closed"
    );
    // An ended view can't be opened again; watching makes a new one.
    let (s, _) = call(
        &r,
        "GET",
        &format!("/api/browser/{sid}/ws"),
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_ne!(watch(&r, id).await, sid);
}
