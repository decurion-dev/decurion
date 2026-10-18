//! "Sign in on this PC" for a Max plan, against a stand-in for Claude Code
//! that prints what `claude setup-token` prints.
#![cfg(unix)]

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::{router, store::Store, AppState, Keys};
use decurion_proto::Profile;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";

struct Hub {
    r: axum::Router,
    vault: decurion_vault::Vault,
    dir: std::path::PathBuf,
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A hub whose "Claude Code" is the shell script `body`.
async fn hub(body: &str) -> Hub {
    use std::os::unix::fs::PermissionsExt;
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let dir = std::env::temp_dir().join(format!(
        "decurion-claude-sign-in-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).expect("dir");
    let program = dir.join("claude");
    std::fs::write(
        &program,
        format!("#!/bin/sh\n[ \"$1\" = setup-token ] || exit 2\n{body}\n"),
    )
    .expect("script");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let vault = decurion_vault::Vault::in_folder(dir.join("vault"));
    let r = router(
        AppState::new(
            store,
            Keys {
                owner: OWNER.into(),
                runner: RUNNER.into(),
            },
            "abc123def456".into(),
            None,
        )
        .with_vault(vault.clone())
        .with_claude_program(program),
    );
    Hub { r, vault, dir }
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
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let resp = r.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

/// Asks for the sign-in's state until `done` says it has moved on.
async fn wait_for(r: &axum::Router, done: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..100 {
        let (s, v) = call(r, "GET", "/api/claude/sign-in", OWNER, Value::Null).await;
        assert_eq!(s, StatusCode::OK);
        if done(&v) {
            return v;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the sign-in never moved on");
}

fn token() -> String {
    format!("sk-ant-oat01-{}", "Zx9_-".repeat(20))
}

#[tokio::test]
async fn signing_in_on_the_pc_saves_the_sign_in_for_that_plan() {
    let t = token();
    let h = hub(&format!(
        "printf '\\033[1mOpening browser to sign in\\033[22m\\r\\n'\n\
         sleep 0.3\n\
         printf 'Your OAuth token (valid for 1 year):\\r\\n\\r\\n\\033[33m{t}\\033[39m\\r\\n\\r\\nStore this token securely.\\r\\n'\n\
         sleep 5"
    ))
    .await;
    let (s, v) = call(
        &h.r,
        "POST",
        "/api/claude/sign-in",
        OWNER,
        json!({ "profile": "B" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["phase"], "waiting");
    assert_eq!(v["profile"], "B");
    let v = wait_for(&h.r, |v| v["phase"] != "waiting").await;
    assert_eq!(v["phase"], "saved", "{v}");
    assert!(
        !v.to_string().contains("sk-ant"),
        "the page never sees the sign-in"
    );
    assert_eq!(h.vault.get_claude(Profile::B).expect("read"), Some(t));
    assert_eq!(h.vault.get_claude(Profile::A).expect("read"), None);
}

#[tokio::test]
async fn claude_s_output_held_back_until_the_cursor_question_is_answered_still_arrives() {
    // Windows' pseudo-terminal asks where the cursor is and passes on
    // nothing until it hears back; this stand-in does the same.
    let t = token();
    let h = hub(&format!(
        "stty raw -echo\n         printf '\\033[6n'\n         answer=$(dd bs=1 count=6 2>/dev/null)\n         [ \"$answer\" = \"$(printf '\\033[1;1R')\" ] || exit 3\n         printf 'Opening browser\\r\\n{t}\\r\\n'\n         sleep 5"
    ))
    .await;
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/claude/sign-in",
        OWNER,
        json!({ "profile": "A" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let v = wait_for(&h.r, |v| v["phase"] != "waiting").await;
    assert_eq!(v["phase"], "saved", "{v}");
    assert_eq!(h.vault.get_claude(Profile::A).expect("read"), Some(t));
}

#[tokio::test]
async fn when_the_page_does_not_open_the_owner_gives_claude_its_code() {
    let h = hub(
        "printf 'Browser didn'\"'\"'t open? Use the url below\\r\\n'\n\
         printf '\\033]8;id=1;https://claude.com/cai/oauth/authorize?code=true&state=s\\007https://claude.com/cai\\033]8;;\\007\\r\\n'\n\
         printf 'Paste code here if prompted > '\n\
         read code\n\
         printf '\\r\\nsk-ant-oat01-%s\\r\\n' \"$code\"\n\
         sleep 5",
    )
    .await;
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/claude/sign-in",
        OWNER,
        json!({ "profile": "A" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let v = wait_for(&h.r, |v| v["wants_code"] == true).await;
    assert_eq!(
        v["link"],
        "https://claude.com/cai/oauth/authorize?code=true&state=s"
    );

    let (s, v) = call(
        &h.r,
        "POST",
        "/api/claude/sign-in/code",
        OWNER,
        json!({ "code": "two words" }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let code = "Zx9_-".repeat(10);
    let (s, v) = call(
        &h.r,
        "POST",
        "/api/claude/sign-in/code",
        OWNER,
        json!({ "code": code }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let v = wait_for(&h.r, |v| v["phase"] != "waiting").await;
    assert_eq!(v["phase"], "saved", "{v}");
    assert_eq!(
        h.vault.get_claude(Profile::A).expect("read"),
        Some(format!("sk-ant-oat01-{code}"))
    );
}

#[tokio::test]
async fn claude_stopping_early_saves_nothing_and_says_so() {
    let h = hub("printf 'Something went wrong\\r\\n'\nexit 1").await;
    call(
        &h.r,
        "POST",
        "/api/claude/sign-in",
        OWNER,
        json!({ "profile": "A" }),
    )
    .await;
    let v = wait_for(&h.r, |v| v["phase"] != "waiting").await;
    assert_eq!(v["phase"], "failed");
    assert!(
        v["message"]
            .as_str()
            .is_some_and(|m| m.contains("Try again") && m.contains("Something went wrong")),
        "{v}"
    );
    assert_eq!(h.vault.get_claude(Profile::A).expect("read"), None);
}

#[tokio::test]
async fn claude_that_cannot_run_is_reported_without_waiting() {
    let h = hub(
        "printf \"'node' is not recognized as an internal or external command\\r\\n\"\nsleep 30",
    )
    .await;
    call(
        &h.r,
        "POST",
        "/api/claude/sign-in",
        OWNER,
        json!({ "profile": "A" }),
    )
    .await;
    let v = wait_for(&h.r, |v| v["phase"] != "waiting").await;
    assert_eq!(v["phase"], "failed");
    assert!(
        v["message"]
            .as_str()
            .is_some_and(|m| m.contains("didn't start")),
        "{v}"
    );
}

#[tokio::test]
async fn one_sign_in_at_a_time_and_cancel_stops_it() {
    let h = hub("sleep 30").await;
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/claude/sign-in",
        OWNER,
        json!({ "profile": "A" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = call(
        &h.r,
        "POST",
        "/api/claude/sign-in",
        OWNER,
        json!({ "profile": "B" }),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/claude/sign-in/cancel",
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let v = wait_for(&h.r, |v| v["phase"] != "waiting").await;
    assert_eq!(v["phase"], "idle");
    let (s, v) = call(
        &h.r,
        "POST",
        "/api/claude/sign-in/code",
        OWNER,
        json!({ "code": "abc" }),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
}

#[tokio::test]
async fn only_the_owner_signs_claude_in() {
    let h = hub("sleep 30").await;
    for (m, p) in [
        ("GET", "/api/claude/sign-in"),
        ("POST", "/api/claude/sign-in"),
        ("POST", "/api/claude/sign-in/code"),
        ("POST", "/api/claude/sign-in/cancel"),
    ] {
        let (s, _) = call(&h.r, m, p, RUNNER, json!({ "profile": "A", "code": "abc" })).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{m} {p}");
    }
}
