//! The checker's receipts: what it ran and what it claims, checked or
//! guessed, are kept with the piece, shown on the goal, handed to the lead's
//! write-up, and never left under a "Not checked" summary.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::{router, store::Store, AppState, Keys};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";

async fn hub() -> (axum::Router, Store) {
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let r = router(AppState::new(
        store.clone(),
        Keys {
            owner: OWNER.into(),
            runner: RUNNER.into(),
        },
        "abc123".into(),
        None,
    ));
    (r, store)
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
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .body(if method == "GET" {
            Body::empty()
        } else {
            Body::from(body.to_string())
        })
        .expect("request");
    let resp = r.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

async fn ok(r: &axum::Router, method: &str, path: &str, key: &str, body: Value) -> Value {
    let (s, v) = call(r, method, path, key, body).await;
    assert!(s.is_success(), "{method} {path}: {s} {v}");
    v
}

async fn view(r: &axum::Router, goal: i64) -> Value {
    ok(r, "GET", &format!("/api/goals/{goal}"), OWNER, json!({})).await
}

/// Claims the next helper job, whatever it is, without finishing it.
async fn claim_any(r: &axum::Router) -> Value {
    let claim = ok(
        r,
        "POST",
        "/api/runner/helper/claim",
        RUNNER,
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    let id = claim["job_id"].as_i64().expect("job id");
    ok(
        r,
        "POST",
        &format!("/api/runner/helper/{id}/started"),
        RUNNER,
        json!({}),
    )
    .await;
    claim
}

async fn finish(r: &axum::Router, claim: &Value, text: &str) {
    let id = claim["job_id"].as_i64().expect("job id");
    ok(
        r,
        "POST",
        &format!("/api/runner/helper/{id}/finish"),
        RUNNER,
        json!({"ok": true, "text": text, "cost_usd": 0.02}),
    )
    .await;
}

/// Claims the next helper job, checks its kind, and finishes it with `text`.
async fn helper(r: &axum::Router, kind: &str, text: &str) -> Value {
    let claim = claim_any(r).await;
    assert_eq!(claim["kind"], kind, "{claim}");
    finish(r, &claim, text).await;
    claim
}

const PLAN: &str = r#"{"say": "Here is my plan.", "plan": {"summary": "A forgot-password link.", "pieces": [{"title": "Reset link", "brief": "Add a reset flow in src/auth.", "done_when": "You can reset your password from the sign-in page."}]}}"#;

const RECEIPTS: &str = r#"I ran the tests and opened the page.
{"pass": true, "summary": "Works.", "issues": [],
 "evidence": [{"ran": "the app's tests", "result": "42 of 42 passed"},
              {"ran": "the sign-in page", "result": "the reset link shows"}],
 "claims": [{"text": "The reset link sends an email.", "how": "checked"},
            {"text": "It works on old phones too.", "how": "guessed"}]}"#;

/// One approved piece whose agent finished, so its check is waiting.
async fn approved_one_piece(r: &axum::Router) -> (i64, i64) {
    let app = ok(
        r,
        "POST",
        "/api/apps",
        OWNER,
        json!({"name":"portal","repo_path":"C:\\repos\\portal","profile":"A","pay_per_use":false}),
    )
    .await["id"]
        .as_i64()
        .expect("app id");
    let g = ok(
        r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "Go"}),
    )
    .await;
    let goal = g["id"].as_i64().expect("goal");
    helper(r, "coordinator", PLAN).await;
    let v = view(r, goal).await;
    let plan_id = v["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["id"].as_i64())
        .expect("plan");
    ok(
        r,
        "POST",
        &format!("/api/goals/{goal}/approve"),
        OWNER,
        json!({"plan_message_id": plan_id}),
    )
    .await;
    let design = claim_any(r).await;
    assert_eq!(design["kind"], "design");
    let id = design["job_id"].as_i64().expect("job id");
    ok(
        r,
        "POST",
        &format!("/api/runner/helper/{id}/finish"),
        RUNNER,
        json!({"ok": true, "text": "done", "cost_usd": 0.02, "page_html": "<h1>x</h1>"}),
    )
    .await;
    let thread = build(r).await;
    (goal, thread)
}

/// The runner takes the piece's work, says it finished, and moves it to
/// review.
async fn build(r: &axum::Router) -> i64 {
    let work = ok(
        r,
        "POST",
        "/api/runner/claim",
        RUNNER,
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    let thread = work["thread"]["id"].as_i64().expect("thread");
    if let Some(m) = work["messages"].as_array().filter(|m| !m.is_empty()) {
        let ids: Vec<Value> = m.iter().map(|m| m["id"].clone()).collect();
        ok(
            r,
            "POST",
            &format!("/api/threads/{thread}/messages/delivered"),
            RUNNER,
            json!({"ids": ids}),
        )
        .await;
    }
    ok(
        r,
        "POST",
        &format!("/api/threads/{thread}/events"),
        RUNNER,
        json!({"kind":"finished","ok":true,"summary":"I added the reset link."}),
    )
    .await;
    ok(
        r,
        "POST",
        &format!("/api/threads/{thread}/move"),
        RUNNER,
        json!({"to":"in_review"}),
    )
    .await;
    thread
}

#[tokio::test]
async fn the_checkers_receipts_reach_the_goal_and_the_write_up() {
    let (r, _) = hub().await;
    let (goal, _) = approved_one_piece(&r).await;
    helper(&r, "review", RECEIPTS).await;
    let v = view(&r, goal).await;
    let piece = &v["pieces"][0];
    assert_eq!(piece["review"], "passed", "{piece}");
    assert_eq!(piece["review_evidence"][0]["ran"], "the app's tests");
    assert_eq!(piece["review_evidence"][0]["result"], "42 of 42 passed");
    assert_eq!(piece["review_evidence"][1]["ran"], "the sign-in page");
    assert_eq!(piece["review_claims"][0]["how"], "checked");
    assert_eq!(piece["review_claims"][1]["how"], "guessed");
    assert_eq!(
        piece["review_claims"][1]["text"],
        "It works on old phones too."
    );

    // The lead's write-up is told what was proven and what was guessed.
    let report = claim_any(&r).await;
    assert_eq!(report["kind"], "coordinator");
    let prompt = report["prompt"].as_str().expect("prompt");
    assert!(
        prompt.contains("Checker ran: the app's tests → 42 of 42 passed"),
        "{prompt}"
    );
    assert!(prompt.contains("Checker says (checked): The reset link sends an email."));
    assert!(prompt.contains("Checker says (guessed): It works on old phones too."));
    assert!(prompt.contains("Keep what the checker checked apart from what it guessed"));
    assert!(!prompt.contains("The research agent wrote"));
    finish(&r, &report, r#"{"say": "Done."}"#).await;
}

#[tokio::test]
async fn a_research_piece_is_reported_with_its_agents_own_words() {
    let (r, store) = hub().await;
    let (goal, thread) = approved_one_piece(&r).await;
    // As if the plan had marked it research and its report were in: the
    // piece settles without a checker (tests/research.rs walks the real flow).
    sqlx::query("UPDATE threads SET research = 1 WHERE id = ?")
        .bind(thread)
        .execute(store.pool())
        .await
        .expect("mark research");
    sqlx::query(
        "INSERT INTO piece_reports (thread_id, goal_id, html) VALUES (?, ?, '<p>found</p>')",
    )
    .bind(thread)
    .bind(goal)
    .execute(store.pool())
    .await
    .expect("a report");
    sqlx::query("UPDATE helper_jobs SET state = 'cancelled' WHERE kind = 'review'")
        .execute(store.pool())
        .await
        .expect("no check");
    sqlx::query("UPDATE goal_pieces SET review = 'building' WHERE thread_id = ?")
        .bind(thread)
        .execute(store.pool())
        .await
        .expect("back to building");
    // The next claim settles the missed piece (recover_checks), then hands out the write-up.
    let report = claim_any(&r).await;
    let prompt = report["prompt"].as_str().expect("prompt");
    assert!(
        prompt.contains("- Reset link: research: report ready, no code changed."),
        "{prompt}"
    );
    assert!(prompt.contains("The research agent wrote: «I added the reset link.»"));
    assert!(prompt.contains("Turn into a goal"));
    finish(&r, &report, r#"{"say": "Your report is ready."}"#).await;
    assert_eq!(view(&r, goal).await["goal"]["state"], "for_review");
}

#[tokio::test]
async fn a_check_that_could_not_be_read_clears_old_receipts() {
    let (r, _) = hub().await;
    let (goal, thread) = approved_one_piece(&r).await;
    // The first check sends the piece back, with receipts.
    helper(
        &r,
        "review",
        r#"{"pass": false, "summary": "The link is missing.", "issues": ["Add the link"],
            "evidence": [{"ran": "the sign-in page", "result": "no reset link"}],
            "claims": [{"text": "The link is not there.", "how": "checked"}]}"#,
    )
    .await;
    let piece = &view(&r, goal).await["pieces"][0];
    assert_eq!(piece["review"], "fixing", "{piece}");
    assert_eq!(piece["review_evidence"][0]["result"], "no reset link");
    // The builder fixes it, and the next check cannot be read twice over.
    assert_eq!(build(&r).await, thread);
    helper(&r, "review", "no json").await;
    helper(&r, "review", "still no json").await;
    let piece = &view(&r, goal).await["pieces"][0];
    assert_eq!(piece["review"], "needs_a_look", "{piece}");
    assert!(
        piece["review_summary"]
            .as_str()
            .is_some_and(|s| s.starts_with("Not checked:")),
        "{piece}"
    );
    assert!(piece.get("review_evidence").is_none(), "{piece}");
    assert!(piece.get("review_claims").is_none(), "{piece}");
}

#[tokio::test]
async fn too_long_a_receipt_is_an_unreadable_answer_asked_again_once() {
    let (r, _) = hub().await;
    let (goal, _) = approved_one_piece(&r).await;
    let long = json!({
        "pass": true,
        "summary": "Works.",
        "evidence": [{"ran": "x".repeat(301), "result": "passed"}],
    })
    .to_string();
    helper(&r, "review", &long).await;
    let piece = &view(&r, goal).await["pieces"][0];
    assert_eq!(piece["review"], "reviewing", "{piece}");
    assert!(piece.get("review_evidence").is_none(), "{piece}");
    // The check is queued once more; a good answer then stands.
    helper(&r, "review", RECEIPTS).await;
    let piece = &view(&r, goal).await["pieces"][0];
    assert_eq!(piece["review"], "passed", "{piece}");
    assert_eq!(piece["review_evidence"][0]["result"], "42 of 42 passed");
}
