//! "Connect your phone": the hub runs Tailscale itself to get its https
//! address, saves it, and pairing codes use it. Tailscale is a small shell
//! script standing in for the real program, so no test touches the
//! network or the computer's own Tailscale. Tests that need the script run
//! on Mac and Linux only; the rest run everywhere.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::{router, store::Store, AppState, Keys};
use http_body_util::BodyExt;
use serde_json::Value;
use std::path::PathBuf;
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";

fn state(store: &Store, public_url: Option<&str>) -> AppState {
    AppState::new(
        store.clone(),
        Keys {
            owner: OWNER.into(),
            runner: RUNNER.into(),
        },
        "abc123def456".into(),
        public_url.map(str::to_owned),
    )
    .with_port(7420)
}

async fn call(r: &axum::Router, method: &str, path: &str, key: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .body(Body::empty())
        .expect("request");
    let resp = r.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("decurion-phone-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("scratch folder");
    dir
}

/// A stand-in for Tailscale: writes each call's arguments to `calls.log`,
/// answers `status --json` with `status`, and runs `serve` as `serve`.
#[cfg(unix)]
fn fake_tailscale(dir: &std::path::Path, status: &str, serve: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(dir.join("status.json"), status).expect("status");
    let log = dir.join("calls.log");
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{log}'\ncase \"$1\" in\n  status) cat '{status}' ;;\n  serve) {serve} ;;\n  *) exit 0 ;;\nesac\n",
        log = log.display(),
        status = dir.join("status.json").display(),
    );
    let path = dir.join("tailscale");
    std::fs::write(&path, script).expect("script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    // Another test starting a program at the moment this file was open for
    // writing can keep it busy for a moment ("text file busy").
    for _ in 0..50 {
        match std::process::Command::new(&path).arg("ready").status() {
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            _ => break,
        }
    }
    std::fs::remove_file(&log).ok();
    path
}

#[cfg(unix)]
fn calls(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join("calls.log")).unwrap_or_default()
}

#[cfg(unix)]
const SIGNED_IN: &str =
    r#"{"BackendState":"Running","Self":{"DNSName":"name.example.ts.net.","HostName":"name"}}"#;

#[cfg(unix)]
#[tokio::test]
async fn connect_sets_up_tailscale_saves_the_address_and_pairing_uses_it() {
    let dir = scratch();
    let ts = fake_tailscale(&dir, SIGNED_IN, "echo 'Available within your tailnet:'");
    let store = Store::open("sqlite::memory:").await.expect("store");
    let r = router(state(&store, None).with_tailscale(ts.clone()));

    let (s, c) = call(&r, "GET", "/api/phone/connection", OWNER).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(c, serde_json::json!({"public_url": null, "https": false}));

    let (s, v) = call(&r, "POST", "/api/phone/connect", OWNER).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v, serde_json::json!({"url": "https://name.example.ts.net"}));
    let log = calls(&dir);
    assert!(log.contains("status --json"), "{log}");
    assert!(log.contains("serve --bg 7420"), "{log}");
    assert!(!log.contains("funnel"), "{log}");

    let (_, c) = call(&r, "GET", "/api/phone/connection", OWNER).await;
    assert_eq!(
        c,
        serde_json::json!({"public_url": "https://name.example.ts.net", "https": true})
    );
    let (s, p) = call(&r, "POST", "/api/pairing", OWNER).await;
    assert_eq!(s, StatusCode::OK);
    let url = p["url"].as_str().expect("url");
    assert!(
        url.starts_with("https://name.example.ts.net/pair?code="),
        "{url}"
    );
    assert!(p["warning"].is_null());

    // A restarted hub on the same database uses the saved address, and a
    // second tap runs nothing.
    std::fs::remove_file(dir.join("calls.log")).ok();
    let again = state(&store, None)
        .with_tailscale(ts.clone())
        .use_saved_public_url()
        .await
        .expect("saved address");
    assert_eq!(
        again.public_url().as_deref(),
        Some("https://name.example.ts.net")
    );
    let r2 = router(again);
    let (_, p) = call(&r2, "POST", "/api/pairing", OWNER).await;
    assert!(p["url"]
        .as_str()
        .is_some_and(|u| u.starts_with("https://name.example.ts.net/pair?code=")));
    let (s, v) = call(&r2, "POST", "/api/phone/connect", OWNER).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["url"], "https://name.example.ts.net");
    assert_eq!(calls(&dir), "");

    // DECURION_PUBLIC_URL, given at start, still wins over the saved one.
    let set = state(&store, Some("https://other.example/"))
        .use_saved_public_url()
        .await
        .expect("state");
    assert_eq!(set.public_url().as_deref(), Some("https://other.example"));
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn a_signed_out_tailscale_asks_the_owner_to_sign_in() {
    let dir = scratch();
    let ts = fake_tailscale(
        &dir,
        r#"{"BackendState":"NeedsLogin","Self":{"DNSName":""}}"#,
        "exit 0",
    );
    let store = Store::open("sqlite::memory:").await.expect("store");
    let r = router(state(&store, None).with_tailscale(ts));
    let (s, v) = call(&r, "POST", "/api/phone/connect", OWNER).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let text = v["error"].as_str().expect("error");
    assert!(
        text.contains("Open Tailscale on this PC and sign in, then try again."),
        "{text}"
    );
    assert!(v["link"].is_null());
    assert!(!calls(&dir).contains("serve"), "serve ran while signed out");
    assert_eq!(store.saved_public_url().await.expect("read"), None);
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn the_link_to_turn_on_https_comes_back_without_waiting() {
    let dir = scratch();
    // Real Tailscale prints the link, then waits until HTTPS is turned on.
    let ts = fake_tailscale(
        &dir,
        SIGNED_IN,
        "printf 'Serve is not enabled on your tailnet.\\nTo enable, visit:\\n\\n         https://login.tailscale.com/f/serve?node=abc123\\n'; exec sleep 60",
    );
    let store = Store::open("sqlite::memory:").await.expect("store");
    let r = router(state(&store, None).with_tailscale(ts));
    let started = std::time::Instant::now();
    let (s, v) = call(&r, "POST", "/api/phone/connect", OWNER).await;
    assert!(started.elapsed().as_secs() < 20, "waited for Tailscale");
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["link"], "https://login.tailscale.com/f/serve?node=abc123");
    assert!(v["error"]
        .as_str()
        .is_some_and(|t| t.starts_with("Tailscale needs HTTPS turned on once.")));
    let (_, c) = call(&r, "GET", "/api/phone/connection", OWNER).await;
    assert_eq!(c["https"], false);
    assert_eq!(store.saved_public_url().await.expect("read"), None);
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn an_https_address_already_set_runs_nothing() {
    let dir = scratch();
    let ts = fake_tailscale(&dir, SIGNED_IN, "exit 0");
    let store = Store::open("sqlite::memory:").await.expect("store");
    let r = router(state(&store, Some("https://hub.test")).with_tailscale(ts));
    let (s, v) = call(&r, "POST", "/api/phone/connect", OWNER).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v, serde_json::json!({"url": "https://hub.test"}));
    assert_eq!(calls(&dir), "", "Tailscale ran");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn without_tailscale_the_owner_gets_the_download_link() {
    let dir = scratch();
    let store = Store::open("sqlite::memory:").await.expect("store");
    let r = router(state(&store, None).with_tailscale(dir.join("no-tailscale-here")));
    let (s, v) = call(&r, "POST", "/api/phone/connect", OWNER).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(v["link"], "https://tailscale.com/download");
    assert!(v["error"]
        .as_str()
        .is_some_and(|t| t.contains("Install Tailscale on this PC and sign in, then try again.")));
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn only_the_owner_connects_a_phone() {
    let dir = scratch();
    let store = Store::open("sqlite::memory:").await.expect("store");
    let r = router(state(&store, None).with_tailscale(dir.join("no-tailscale-here")));
    let (s, _) = call(&r, "POST", "/api/phone/connect", RUNNER).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(&r, "GET", "/api/phone/connection", RUNNER).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    std::fs::remove_dir_all(&dir).ok();
}
