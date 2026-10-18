//! Protected parts of an app: a piece that changes them waits for the owner
//! even when the checker passes it. The runner's side, real git included,
//! is played by the test.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::{router, store::Store, AppState, Keys};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";

const LIST: &str = "# The parts I look at myself\nsrc/walls/\nsecret.txt\n";

const PLAN: &str = r#"{"say": "Here is my plan.", "plan": {"summary": "A small change.", "pieces": [{"title": "Change", "brief": "Change the app.", "done_when": "It is changed."}]}}"#;

const PASS: &str = r#"{"pass": true, "summary": "Looks good."}"#;

struct Hub {
    r: axum::Router,
    _store: Store,
    dir: PathBuf,
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn hub() -> Hub {
    let dir = std::env::temp_dir().join(format!("decurion-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let r = router(
        AppState::new(
            store.clone(),
            Keys {
                owner: OWNER.into(),
                runner: RUNNER.into(),
            },
            "abc123".into(),
            None,
        )
        .with_files(dir.join("saved")),
    );
    Hub {
        r,
        _store: store,
        dir,
    }
}

async fn send(
    r: &axum::Router,
    method: &str,
    path: &str,
    body: Body,
) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
    let key = if path.starts_with("/api/runner")
        || path.contains("/move")
        || path.contains("/events")
        || path.contains("/delivered")
    {
        RUNNER
    } else {
        OWNER
    };
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .body(body)
        .expect("request");
    let resp = r.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, bytes.to_vec(), headers)
}

async fn ok(r: &axum::Router, method: &str, path: &str, body: Value) -> Value {
    let body = if method == "GET" {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let (s, bytes, _) = send(r, method, path, body).await;
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    assert!(s.is_success(), "{method} {path}: {s} {v}");
    v
}

/// Runs git in `dir`, returning what it printed.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn write(dir: &Path, path: &str, text: &str) {
    let p = dir.join(path);
    std::fs::create_dir_all(p.parent().expect("parent")).expect("dirs");
    std::fs::write(p, text).expect("write");
}

/// The owner's app checkout, with or without a protected list.
fn app_repo(dir: &Path, list: Option<&str>) -> PathBuf {
    let repo = dir.join("app");
    std::fs::create_dir_all(&repo).expect("repo dir");
    git(&repo, &["init", "--quiet", "-b", "main"]);
    write(&repo, "README.md", "hello\n");
    write(&repo, "src/app.rs", "fn main() {}\n");
    write(&repo, "src/walls/box.rs", "// the box\n");
    write(&repo, "secret.txt", "shh\n");
    if let Some(l) = list {
        write(&repo, ".decurion/protected-paths", l);
    }
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "first"]);
    repo
}

async fn helper(r: &axum::Router, kind: &str, text: &str) -> Value {
    let claim = ok(
        r,
        "POST",
        "/api/runner/helper/claim",
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(claim["kind"], kind, "{claim}");
    let id = claim["job_id"].as_i64().expect("job id");
    let (s, _, _) = send(
        r,
        "POST",
        &format!("/api/runner/helper/{id}/started"),
        Body::from("{}"),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, b, _) = send(
        r,
        "POST",
        &format!("/api/runner/helper/{id}/finish"),
        Body::from(json!({"ok": true, "text": text, "cost_usd": 0.01}).to_string()),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{}", String::from_utf8_lossy(&b));
    claim
}

/// A goal with one approved piece, claimed by the PC runner. Returns the
/// goal, the piece's thread and its branch.
async fn claimed_piece(h: &Hub, list: Option<&str>) -> (i64, i64, String) {
    let repo = app_repo(&h.dir, list);
    let app = ok(
        &h.r,
        "POST",
        "/api/apps",
        json!({"name": "demo", "repo_path": repo.to_string_lossy(), "profile": "A"}),
    )
    .await["id"]
        .as_i64()
        .expect("app id");
    let goal = ok(
        &h.r,
        "POST",
        "/api/goals",
        json!({"app_id": app, "text": "Go"}),
    )
    .await["id"]
        .as_i64()
        .expect("goal id");
    helper(&h.r, "coordinator", PLAN).await;
    let v = ok(&h.r, "GET", &format!("/api/goals/{goal}"), Value::Null).await;
    let plan_id = v["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["id"].as_i64())
        .expect("plan id");
    ok(
        &h.r,
        "POST",
        &format!("/api/goals/{goal}/approve"),
        json!({"plan_message_id": plan_id}),
    )
    .await;
    helper(&h.r, "design", "done").await;
    let work = ok(
        &h.r,
        "POST",
        "/api/runner/claim",
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    let thread = work["thread"]["id"].as_i64().expect("thread");
    let branch = work["thread"]["branch"]
        .as_str()
        .expect("branch")
        .to_owned();
    (goal, thread, branch)
}

/// Builds the piece the way a runner does: from the hub's starting point,
/// with `changes` committed on the branch, saved as a checkpoint.
async fn save_work(h: &Hub, thread: i64, branch: &str, changes: &[(&str, Option<&str>)]) {
    let (s, bundle, headers) = send(
        &h.r,
        "GET",
        &format!("/api/runner/threads/{thread}/base"),
        Body::empty(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let base = headers["x-decurion-base"].to_str().expect("sha").to_owned();
    let xfer = h.dir.join(format!("xfer-{thread}"));
    std::fs::create_dir_all(&xfer).expect("xfer");
    let base_bundle = xfer.join("base.bundle");
    std::fs::write(&base_bundle, bundle).expect("write");
    let wt = h.dir.join(format!("work-{thread}"));
    std::fs::create_dir_all(&wt).expect("work dir");
    git(&wt, &["init", "--quiet"]);
    git(
        &wt,
        &[
            "fetch",
            "--quiet",
            &base_bundle.to_string_lossy(),
            &format!("refs/heads/decurion-base:refs/heads/{branch}"),
        ],
    );
    git(&wt, &["checkout", "--quiet", branch]);
    for (path, text) in changes {
        match text {
            Some(t) => write(&wt, path, t),
            None => {
                git(&wt, &["rm", "--quiet", path]);
            }
        }
    }
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "--quiet", "-m", "the work"]);
    let head = git(&wt, &["rev-parse", "HEAD"]);
    let code = xfer.join("code.bundle");
    git(
        &wt,
        &[
            "bundle",
            "create",
            "--quiet",
            &code.to_string_lossy(),
            &format!("{base}..refs/heads/{branch}"),
        ],
    );
    let parts = [
        ("code", std::fs::read(&code).expect("read")),
        ("session", b"SESSION TAR".to_vec()),
    ];
    for (part, data) in parts {
        let (s, _, _) = send(
            &h.r,
            "PUT",
            &format!("/api/runner/threads/{thread}/upload/{part}"),
            Body::from(data),
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT);
    }
    ok(
        &h.r,
        "POST",
        &format!("/api/runner/threads/{thread}/checkpoint"),
        json!({"head": head, "base": base}),
    )
    .await;
}

async fn to_review(h: &Hub, thread: i64) {
    ok(
        &h.r,
        "POST",
        &format!("/api/threads/{thread}/events"),
        json!({"kind":"message","text":"done"}),
    )
    .await;
    ok(
        &h.r,
        "POST",
        &format!("/api/threads/{thread}/move"),
        json!({"to":"in_review"}),
    )
    .await;
}

async fn piece(h: &Hub, goal: i64) -> (Value, Vec<String>) {
    let v = ok(&h.r, "GET", &format!("/api/goals/{goal}"), Value::Null).await;
    let notes = v["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter(|m| m["body"]["kind"] == "note")
        .filter_map(|m| m["body"]["text"].as_str().map(str::to_owned))
        .collect();
    (v["pieces"][0].clone(), notes)
}

#[tokio::test]
async fn a_piece_touching_protected_parts_is_held_even_when_the_checker_passes_it() {
    let h = hub().await;
    let (goal, thread, branch) = claimed_piece(&h, Some(LIST)).await;
    save_work(
        &h,
        thread,
        &branch,
        &[
            ("src/app.rs", Some("fn main() { run() }\n")),
            ("src/walls/box.rs", Some("// a looser box\n")),
        ],
    )
    .await;
    to_review(&h, thread).await;

    // A failing check goes back to the builder as always.
    helper(
        &h.r,
        "review",
        r#"{"pass": false, "summary": "Not yet.", "issues": ["Add a test"]}"#,
    )
    .await;
    let (p, _) = piece(&h, goal).await;
    assert_eq!(p["review"], "fixing");

    // The builder answers; the checker passes it; the hub holds it.
    let work = ok(
        &h.r,
        "POST",
        "/api/runner/claim",
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    ok(
        &h.r,
        "POST",
        &format!("/api/threads/{thread}/messages/delivered"),
        json!({"ids": [work["messages"][0]["id"]]}),
    )
    .await;
    to_review(&h, thread).await;
    helper(&h.r, "review", PASS).await;
    let (p, notes) = piece(&h, goal).await;
    assert_eq!(p["review"], "needs_a_look", "{p}");
    let summary = p["review_summary"].as_str().expect("summary");
    assert!(
        summary.starts_with(
            "Held for you: this changes protected parts of the app (src/walls/box.rs). The checker passed it"
        ),
        "{summary}"
    );
    assert!(summary.ends_with("Looks good."), "{summary}");
    assert!(
        notes
            .iter()
            .any(|n| n.contains("Change passed the check") && n.contains("held for you")),
        "{notes:?}"
    );
    // The plan's approval does not cover a held piece: it waits in review
    // for the owner's own tap, and nothing goes to GitHub.
    assert_eq!(p["thread_state"], "in_review", "{p}");
    assert!(p.get("github").is_none(), "{p}");
    // Its work panel says so.
    let of = ok(
        &h.r,
        "GET",
        &format!("/api/threads/{thread}/goal"),
        Value::Null,
    )
    .await;
    assert_eq!(of["goal_id"], goal, "{of}");
    assert_eq!(of["held"], true, "{of}");
    assert_eq!(of["by_plan"], false, "{of}");
}

#[tokio::test]
async fn taking_a_pattern_off_the_list_does_not_unprotect_it() {
    let h = hub().await;
    let (goal, thread, branch) = claimed_piece(&h, Some(LIST)).await;
    save_work(
        &h,
        thread,
        &branch,
        &[
            (".decurion/protected-paths", Some("src/walls/\n")),
            ("secret.txt", Some("everyone may know\n")),
        ],
    )
    .await;
    to_review(&h, thread).await;
    helper(&h.r, "review", PASS).await;
    let (p, _) = piece(&h, goal).await;
    assert_eq!(p["review"], "needs_a_look", "{p}");
    let summary = p["review_summary"].as_str().expect("summary");
    assert!(
        summary.contains("(.decurion/protected-paths, secret.txt)"),
        "{summary}"
    );
}

#[tokio::test]
async fn deleting_the_list_is_held_too() {
    let h = hub().await;
    let (goal, thread, branch) = claimed_piece(&h, Some(LIST)).await;
    save_work(&h, thread, &branch, &[(".decurion/protected-paths", None)]).await;
    to_review(&h, thread).await;
    helper(&h.r, "review", PASS).await;
    let (p, _) = piece(&h, goal).await;
    assert_eq!(p["review"], "needs_a_look", "{p}");
}

#[tokio::test]
async fn a_piece_touching_only_unprotected_files_passes() {
    let h = hub().await;
    let (goal, thread, branch) = claimed_piece(&h, Some(LIST)).await;
    save_work(
        &h,
        thread,
        &branch,
        &[
            ("src/app.rs", Some("fn main() { run() }\n")),
            ("src/walls.md", Some("not inside the folder\n")),
        ],
    )
    .await;
    to_review(&h, thread).await;
    helper(&h.r, "review", PASS).await;
    let (p, notes) = piece(&h, goal).await;
    assert_eq!(p["review"], "passed", "{p}");
    assert_eq!(p["review_summary"], "Looks good.");
    assert!(
        notes
            .iter()
            .any(|n| n == "Change passed the check and is on its way to GitHub."),
        "{notes:?}"
    );
    // The owner approved the plan, so a piece that passes is accepted and
    // goes to GitHub without another tap.
    assert_eq!(p["thread_state"], "done", "{p}");
    assert_eq!(p["github"]["state"], "waiting", "{p}");
    // Its work panel says why it did not wait for Accept, naming the goal.
    let of = ok(
        &h.r,
        "GET",
        &format!("/api/threads/{thread}/goal"),
        Value::Null,
    )
    .await;
    assert_eq!(of["goal_id"], goal, "{of}");
    assert_eq!(of["goal_title"], "Go", "{of}");
    assert_eq!(of["review"], "passed", "{of}");
    assert_eq!(of["by_plan"], true, "{of}");
    assert_eq!(of["held"], false, "{of}");
    assert_eq!(of["sends_checked"], true, "{of}");
}

#[tokio::test]
async fn work_added_straight_to_the_board_is_part_of_no_goal() {
    let h = hub().await;
    let (_, piece_thread, _) = claimed_piece(&h, None).await;
    let app = ok(
        &h.r,
        "GET",
        &format!("/api/threads/{piece_thread}"),
        Value::Null,
    )
    .await["app_id"]
        .clone();
    let t = ok(
        &h.r,
        "POST",
        "/api/threads",
        json!({"app_id": app, "title": "Just do it", "prompt": "Change the app."}),
    )
    .await["id"]
        .as_i64()
        .expect("thread id");
    let of = ok(&h.r, "GET", &format!("/api/threads/{t}/goal"), Value::Null).await;
    assert!(of.is_null(), "{of}");
    // A piece not yet checked went nowhere by the plan yet.
    let of = ok(
        &h.r,
        "GET",
        &format!("/api/threads/{piece_thread}/goal"),
        Value::Null,
    )
    .await;
    assert_eq!(of["review"], "building", "{of}");
    assert_eq!(of["by_plan"], false, "{of}");

    // Only the owner's screens ask; a runner key is turned away.
    let req = Request::builder()
        .uri(format!("/api/threads/{piece_thread}/goal"))
        .header(header::AUTHORIZATION, format!("Bearer {RUNNER}"))
        .body(Body::empty())
        .expect("request");
    let resp = h.r.clone().oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let (s, _, _) = send(&h.r, "GET", "/api/threads/999999/goal", Body::empty()).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_app_without_a_list_passes_as_before() {
    let h = hub().await;
    let (goal, thread, branch) = claimed_piece(&h, None).await;
    save_work(&h, thread, &branch, &[("src/walls/box.rs", Some("// x\n"))]).await;
    to_review(&h, thread).await;
    helper(&h.r, "review", PASS).await;
    let (p, _) = piece(&h, goal).await;
    assert_eq!(p["review"], "passed", "{p}");
}

#[tokio::test]
async fn a_piece_that_cannot_be_checked_is_held() {
    let h = hub().await;
    let (goal, thread, _) = claimed_piece(&h, Some(LIST)).await;
    // The starting point is known, but no saved copy of the work ever
    // reached the hub, so it cannot tell what changed.
    let (s, _, _) = send(
        &h.r,
        "GET",
        &format!("/api/runner/threads/{thread}/base"),
        Body::empty(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    to_review(&h, thread).await;
    helper(&h.r, "review", PASS).await;
    let (p, notes) = piece(&h, goal).await;
    assert_eq!(p["review"], "needs_a_look", "{p}");
    let summary = p["review_summary"].as_str().expect("summary");
    assert!(
        summary.starts_with("Held for you: the hub could not check"),
        "{summary}"
    );
    assert!(
        notes
            .iter()
            .any(|n| n.contains("could not check its protected parts")),
        "{notes:?}"
    );
}
