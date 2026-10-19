//! One-tap deploy: after a merge, an app whose rules allow deploys gets one
//! Ready to deploy card for the exact commit on its main line, and one tap
//! runs its steps in the hub's own clean copy. `origin` is a local bare repo
//! and the steps are git itself, so no test reaches the network and every
//! step runs the same on Linux and Windows.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::{router, store::Store, AppState, Keys};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";

struct Hub {
    r: axum::Router,
    store: Store,
    dir: PathBuf,
    repo: PathBuf,
    bare: PathBuf,
    app: i64,
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

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

/// A merge on the remote's main line: a commit in the owner's folder,
/// pushed to `origin`. Returns the new commit.
fn merge(h: &Hub, file: &str, text: &str, title: &str) -> String {
    write(&h.repo, file, text);
    git(&h.repo, &["add", "."]);
    git(&h.repo, &["commit", "--quiet", "-m", title]);
    git(&h.repo, &["push", "--quiet", "origin", "main"]);
    git(&h.bare, &["rev-parse", "main"])
}

async fn hub() -> Hub {
    let dir = std::env::temp_dir().join(format!("decurion-deploy-{}", uuid::Uuid::new_v4()));
    let repo = dir.join("app");
    let bare = dir.join("origin.git");
    std::fs::create_dir_all(&repo).expect("repo dir");
    std::fs::create_dir_all(&bare).expect("bare dir");
    git(&bare, &["init", "--quiet", "--bare", "-b", "main"]);
    git(&repo, &["init", "--quiet", "-b", "main"]);
    write(&repo, "README.md", "hello\n");
    write(&repo, "scripts/deploy.mjs", "console.log('v1')\n");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "First version"]);
    git(&repo, &["remote", "add", "origin", &bare.to_string_lossy()]);
    git(&repo, &["push", "--quiet", "origin", "main"]);
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let state = AppState::new(
        store.clone(),
        Keys {
            owner: OWNER.into(),
            runner: RUNNER.into(),
        },
        "abc123".into(),
        None,
    )
    .with_files(dir.join("saved"));
    let r = router(state);
    let app = ok(
        &r,
        OWNER,
        "POST",
        "/api/apps",
        json!({"name": "EMR", "repo_path": repo.to_string_lossy(), "profile": "A"}),
    )
    .await["id"]
        .as_i64()
        .expect("app id");
    Hub {
        r,
        store,
        dir,
        repo,
        bare,
        app,
    }
}

/// Saves the app's deploy rule the way the app's rules screen does.
async fn rule(h: &Hub, on: bool, steps: &str) {
    sqlx::query("UPDATE apps SET can_deploy = ?, deploy_steps = ?, deploy_site = ? WHERE id = ?")
        .bind(on)
        .bind(steps)
        .bind("https://app.example.com")
        .bind(h.app)
        .execute(h.store.pool())
        .await
        .expect("rule");
}

async fn call(
    r: &axum::Router,
    key: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let body = if method == "GET" {
        Body::empty()
    } else {
        Body::from(body.to_string())
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
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

async fn ok(r: &axum::Router, key: &str, method: &str, path: &str, body: Value) -> Value {
    let (s, v) = call(r, key, method, path, body).await;
    assert!(s.is_success(), "{method} {path}: {s} {v}");
    v
}

/// Looks at the remote now and returns the app's card.
async fn check(h: &Hub) -> Value {
    let v = ok(
        &h.r,
        OWNER,
        "POST",
        &format!("/api/apps/{}/deploy/check", h.app),
        json!({}),
    )
    .await;
    v.as_array()
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or(Value::Null)
}

async fn tap(h: &Hub, card: &Value, receipt: &str) -> (StatusCode, Value) {
    call(
        &h.r,
        OWNER,
        "POST",
        &format!("/api/deploys/{}/go", card["id"]),
        json!({"commit": card["commit"], "receipt": receipt}),
    )
    .await
}

/// Waits for deploy `id` to finish and returns its card.
async fn ended(h: &Hub, id: i64) -> decurion_hub::deploy::Card {
    for _ in 0..600 {
        let c = h.store.deploy_card(id).await.expect("card");
        if c.state != "deploying" {
            return c;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("deploy {id} never finished");
}

#[tokio::test]
async fn a_merge_shows_one_card_and_one_tap_runs_the_steps_on_that_commit() {
    let h = hub().await;
    rule(&h, true, "git rev-parse HEAD\ngit status --short --branch").await;
    let head = git(&h.bare, &["rev-parse", "main"]);

    let card = check(&h).await;
    assert_eq!(card["state"], "waiting", "{card}");
    assert_eq!(card["commit"], head.as_str());
    assert_eq!(card["status"], "Ready to deploy");
    assert_eq!(card["button"], "Deploy");
    assert_eq!(card["site"], "https://app.example.com");
    assert_eq!(card["changes"], json!(["First version"]));

    // The Today strip says so too, and the row waits on the owner.
    let today = ok(&h.r, OWNER, "GET", "/api/today", json!({})).await;
    assert_eq!(today[0]["deploy"]["id"], card["id"]);
    assert_eq!(today[0]["tone"], "needs_you");
    assert_eq!(today[0]["line"], "Merged · ready to deploy");

    let (s, started) = tap(&h, &card, "tap-deploy-0001").await;
    assert_eq!(s, StatusCode::OK, "{started}");
    assert_eq!(started["state"], "deploying");
    let done = ended(&h, card["id"].as_i64().expect("id")).await;
    assert_eq!(done.state, "live", "{:?}", done.error);
    assert_eq!(done.status, "Live");
    // The steps ran in a clean copy of exactly that commit, on its main line.
    let tail = done.tail.clone().unwrap_or_default();
    assert!(tail.contains(&head), "{tail}");
    assert!(tail.contains("## main...origin/main"), "{tail}");
    let log = call(
        &h.r,
        OWNER,
        "GET",
        &format!("/api/deploys/{}/log", done.id),
        json!({}),
    )
    .await
    .1;
    let log = log.as_str().expect("text");
    assert!(
        log.contains("$ git rev-parse HEAD") && log.contains("Deploy finished."),
        "{log}"
    );
    // Nothing was written into the owner's folder.
    assert_eq!(git(&h.repo, &["status", "--porcelain"]), "");

    // A resent tap starts nothing again.
    let (s, again) = tap(&h, &card, "tap-deploy-0001").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(again["state"], "live");
    let history = h.store.deploy_history(done.id).await.expect("history");
    let whats: Vec<&str> = history.iter().map(|(_, w)| w.as_str()).collect();
    assert_eq!(whats, vec!["ready to deploy", "Deploy tapped", "live"]);

    // Nothing new on the main line: no new card, the live one stays a day.
    let card = check(&h).await;
    assert_eq!(card["state"], "live");
    let today = ok(&h.r, OWNER, "GET", "/api/today", json!({})).await;
    assert_eq!(today[0]["line"], "Live");
    assert_eq!(today[0]["status"], "Live");

    // The next merge makes a new card that lists only what is new.
    let next = merge(&h, "README.md", "hello again\n", "Fix login");
    let card = check(&h).await;
    assert_eq!(card["state"], "waiting");
    assert_eq!(card["commit"], next.as_str());
    assert_eq!(card["changes"], json!(["Fix login"]));
    assert_eq!(card["line"], "1 change: Fix login");
}

#[tokio::test]
async fn a_newer_merge_moves_the_card_and_a_stale_tap_is_refused() {
    let h = hub().await;
    rule(&h, true, "git --version").await;
    let first = check(&h).await;
    let second = merge(&h, "README.md", "two\n", "Second");
    let moved = check(&h).await;
    assert_eq!(moved["id"], first["id"], "one card, moved on");
    assert_eq!(moved["commit"], second.as_str());

    // The owner's screen still showed the first commit.
    let (s, v) = tap(&h, &first, "tap-stale-0001").await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(v.as_str().unwrap_or("").contains("newer change"), "{v}");
    assert_eq!(
        h.store
            .deploy_card(first["id"].as_i64().expect("id"))
            .await
            .expect("card")
            .state,
        "waiting"
    );

    // Not now hides it until the next merge.
    ok(
        &h.r,
        OWNER,
        "POST",
        &format!("/api/deploys/{}/hide", moved["id"]),
        json!({"receipt": "hide-0000001"}),
    )
    .await;
    assert_eq!(check(&h).await, Value::Null);
    merge(&h, "README.md", "three\n", "Third");
    assert_eq!(check(&h).await["state"], "waiting");
}

#[tokio::test]
async fn a_failed_step_says_which_and_try_again_runs_it_again() {
    let h = hub().await;
    rule(&h, true, "git --version\ngit no-such-command").await;
    let card = check(&h).await;
    let (s, _) = tap(&h, &card, "tap-fail-00001").await;
    assert_eq!(s, StatusCode::OK);
    let done = ended(&h, card["id"].as_i64().expect("id")).await;
    assert_eq!(done.state, "failed");
    assert_eq!(done.button.as_deref(), Some("Try again"));
    let err = done.error.clone().unwrap_or_default();
    assert!(
        err.starts_with("Step 2 of 2 stopped with an error"),
        "{err}"
    );
    assert!(done.tail.unwrap_or_default().contains("no-such-command"));
    let today = ok(&h.r, OWNER, "GET", "/api/today", json!({})).await;
    assert_eq!(today[0]["tone"], "needs_you");

    // The owner fixes the steps and taps Try again on the same card.
    rule(&h, true, "git --version").await;
    let (s, v) = tap(&h, &card, "tap-fail-00002").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(ended(&h, done.id).await.state, "live");

    // A program that is not on the PC is said in plain words.
    merge(&h, "README.md", "x\n", "Another");
    rule(&h, true, "no-such-program-decurion --x").await;
    let card = check(&h).await;
    tap(&h, &card, "tap-fail-00003").await;
    let done = ended(&h, card["id"].as_i64().expect("id")).await;
    assert!(
        done.error
            .unwrap_or_default()
            .contains("could not find no-such-program-decurion"),
        "plain words for a missing program"
    );
}

#[tokio::test]
async fn a_changed_deploy_script_is_named_on_the_card_before_the_tap() {
    let h = hub().await;
    rule(&h, true, "git hash-object scripts/deploy.mjs").await;
    let card = check(&h).await;
    assert_eq!(
        card["steps_changed"],
        json!([]),
        "first deploy: nothing to compare"
    );
    tap(&h, &card, "tap-steps-0001").await;
    assert_eq!(
        ended(&h, card["id"].as_i64().expect("id")).await.state,
        "live"
    );

    merge(
        &h,
        "scripts/deploy.mjs",
        "console.log('v2')\n",
        "Change the deploy script",
    );
    let card = check(&h).await;
    assert_eq!(card["steps_changed"], json!(["scripts/deploy.mjs"]));
    assert_eq!(card["button"], "Deploy new steps");
    assert_eq!(card["line"], "Deploy steps changed: scripts/deploy.mjs");

    // Changed again after the card was drawn: the tap runs nothing.
    let pushed = merge(&h, "scripts/deploy.mjs", "console.log('v3')\n", "And again");
    sqlx::query("UPDATE deploys SET commit_id = ? WHERE id = ?")
        .bind(&pushed)
        .bind(card["id"].as_i64())
        .execute(h.store.pool())
        .await
        .expect("as if the card had moved without a new look");
    let stale = json!({"id": card["id"], "commit": pushed});
    tap(&h, &stale, "tap-steps-0002").await;
    let done = ended(&h, card["id"].as_i64().expect("id")).await;
    assert_eq!(done.state, "failed");
    assert!(done.error.unwrap_or_default().contains("steps changed"));
}

#[tokio::test]
async fn deploys_need_the_apps_rule_and_the_owner() {
    let h = hub().await;
    rule(&h, false, "git --version").await;
    let (s, v) = call(
        &h.r,
        OWNER,
        "POST",
        &format!("/api/apps/{}/deploy/check", h.app),
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(v.as_str().unwrap_or("").contains("Turn them on"));

    rule(&h, true, "git --version").await;
    let card = check(&h).await;
    // Rules turned off again: the card goes and a tap is refused.
    rule(&h, false, "git --version").await;
    assert_eq!(
        ok(&h.r, OWNER, "GET", "/api/deploys", json!({})).await,
        json!([])
    );
    let (s, _) = tap(&h, &card, "tap-off-000001").await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Steps with shell marks never count as a rule.
    rule(&h, true, "git --version && git status").await;
    let (s, _) = tap(&h, &card, "tap-off-000002").await;
    assert_eq!(s, StatusCode::CONFLICT);

    rule(&h, true, "git --version").await;
    for (method, path) in [
        ("GET", "/api/deploys".to_owned()),
        ("POST", format!("/api/apps/{}/deploy/check", h.app)),
        ("POST", format!("/api/deploys/{}/go", card["id"])),
        ("POST", format!("/api/deploys/{}/hide", card["id"])),
        ("GET", format!("/api/deploys/{}/log", card["id"])),
    ] {
        let (s, _) = call(
            &h.r,
            RUNNER,
            method,
            &path,
            json!({"commit": card["commit"]}),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{method} {path}");
    }
    assert_eq!(
        h.store
            .deploy_card(card["id"].as_i64().expect("id"))
            .await
            .expect("card")
            .state,
        "waiting"
    );
}

#[tokio::test]
async fn a_deploy_left_running_by_a_stopped_hub_is_marked_failed_not_run_again() {
    let h = hub().await;
    rule(&h, true, "git --version").await;
    let card = check(&h).await;
    sqlx::query("UPDATE deploys SET state = 'deploying' WHERE id = ?")
        .bind(card["id"].as_i64())
        .execute(h.store.pool())
        .await
        .expect("as if running");
    assert_eq!(h.store.recover_deploys().await.expect("recover"), 1);
    let c = h
        .store
        .deploy_card(card["id"].as_i64().expect("id"))
        .await
        .expect("card");
    assert_eq!(c.state, "failed");
    assert!(c.error.unwrap_or_default().contains("Check the site"));
    assert_eq!(c.button.as_deref(), Some("Try again"));
}

#[tokio::test]
async fn a_merged_goal_is_ready_to_deploy_until_a_deploy_carries_it_live() {
    let h = hub().await;
    rule(&h, true, "git rev-parse HEAD").await;
    let head = merge(&h, "README.md", "fixed\n", "Fix login");
    // A goal whose one pull request GitHub merged as `head`.
    let thread = ok(
        &h.r,
        OWNER,
        "POST",
        "/api/threads",
        json!({"app_id": h.app, "title": "Fix login", "prompt": "Fix it"}),
    )
    .await["id"]
        .as_i64()
        .expect("thread id");
    let goal: i64 = sqlx::query_scalar(
        "INSERT INTO goals (app_id, title, state, session_id) VALUES (?, 'Fix login', 'for_review', 's') RETURNING id",
    )
    .bind(h.app)
    .fetch_one(h.store.pool())
    .await
    .expect("goal");
    sqlx::query(
        "INSERT INTO plan_items (app_id, position, kind, goal_id, title, source)
         VALUES (?, 1, 'goal', ?, 'Fix login', 'owner')",
    )
    .bind(h.app)
    .bind(goal)
    .execute(h.store.pool())
    .await
    .expect("plan row");
    sqlx::query(
        "INSERT INTO goal_pieces (thread_id, goal_id, title, done_when, review, merged_at)
         VALUES (?, ?, 'Fix login', 'It works', 'passed', '2026-10-18T10:00:00Z')",
    )
    .bind(thread)
    .bind(goal)
    .execute(h.store.pool())
    .await
    .expect("piece");
    sqlx::query(
        "INSERT INTO pr_watches (thread_id, app_id, owner, repo, number, url, branch, head, state, event, event_at, merge_commit)
         VALUES (?, ?, 'me', 'app', 1, 'https://github.com/me/app/pull/1', 'decurion/x', ?, 'merged', 'Merged', '2026-10-18T10:00:00Z', ?)",
    )
    .bind(thread)
    .bind(h.app)
    .bind(&head)
    .bind(&head)
    .execute(h.store.pool())
    .await
    .expect("watch");

    let last_step = |plan: &Value| {
        let row = plan["items"]
            .as_array()
            .and_then(|g| {
                g.iter()
                    .find(|r| r["kind"] == "goal" && r["goal"]["id"] == goal)
            })
            .cloned()
            .unwrap_or(Value::Null);
        let steps = row["steps"].as_array().cloned().unwrap_or_default();
        steps.last().cloned().unwrap_or(Value::Null)
    };
    let view = ok(&h.r, OWNER, "GET", &format!("/api/goals/{goal}"), json!({})).await;
    assert_eq!(view["live"], false, "{view}");
    assert_eq!(view["road"]["label"], "Needs you · deploy", "{view}");
    assert_eq!(
        view["goal"]["state"], "for_review",
        "merged is not live: {view}"
    );
    let plan = ok(
        &h.r,
        OWNER,
        "GET",
        &format!("/api/apps/{}/plan", h.app),
        json!({}),
    )
    .await;
    let step = last_step(&plan);
    assert_eq!(step["title"], "Merged · ready to deploy", "{plan}");
    assert_eq!(step["state"], "you");

    let card = check(&h).await;
    let (s, _) = tap(&h, &card, "tap-deploy-live-01").await;
    assert_eq!(s, StatusCode::OK);
    let done = ended(&h, card["id"].as_i64().expect("id")).await;
    assert_eq!(done.state, "live", "{:?}", done.error);

    let view = ok(&h.r, OWNER, "GET", &format!("/api/goals/{goal}"), json!({})).await;
    assert_eq!(view["live"], true, "{view}");
    // Live was its last step, so the goal closed by itself.
    assert_eq!(view["goal"]["state"], "done", "{view}");
    assert_eq!(view["road"]["label"], "Live", "{view}");
    let plan = ok(
        &h.r,
        OWNER,
        "GET",
        &format!("/api/apps/{}/plan", h.app),
        json!({}),
    )
    .await;
    let step = last_step(&plan);
    assert_eq!(step["title"], "Live");
    assert_eq!(step["state"], "done");

    // With deploys off, the goal is just done, as before.
    rule(&h, false, "git rev-parse HEAD").await;
    let view = ok(&h.r, OWNER, "GET", &format!("/api/goals/{goal}"), json!({})).await;
    assert!(view.get("live").is_none(), "{view}");
}
