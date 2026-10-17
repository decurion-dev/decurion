//! A goal from the owner's first words to reviewed work, with the runner's
//! side played by the test.

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

async fn raw(
    r: &axum::Router,
    method: &str,
    path: &str,
    key: &str,
    body: Value,
) -> axum::response::Response {
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
    r.clone().oneshot(req).await.expect("response")
}

async fn call(
    r: &axum::Router,
    method: &str,
    path: &str,
    key: &str,
    body: Value,
) -> (StatusCode, Value) {
    let resp = raw(r, method, path, key, body).await;
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

async fn app(r: &axum::Router, pay_per_use: bool) -> i64 {
    let v = ok(
        r,
        "POST",
        "/api/apps",
        OWNER,
        json!({"name":"portal","repo_path":"C:\\repos\\portal","profile":"A","pay_per_use":pay_per_use}),
    )
    .await;
    v["id"].as_i64().expect("app id")
}

/// Claims the next helper job, checks its kind, and finishes it with `text`.
async fn helper(r: &axum::Router, kind: &str, text: &str, page: Option<&str>) -> Value {
    let (s, claim) = call(
        r,
        "POST",
        "/api/runner/helper/claim",
        RUNNER,
        json!({"runner":"local"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "expected a {kind} job, got {claim}");
    assert_eq!(claim["kind"], kind, "{claim}");
    let id = claim["job_id"].as_i64().expect("job id");
    let (s, _) = call(
        r,
        "POST",
        &format!("/api/runner/helper/{id}/started"),
        RUNNER,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let mut res = json!({"ok": true, "text": text, "cost_usd": 0.02});
    if let Some(p) = page {
        res["page_html"] = json!(p);
    }
    let (s, v) = call(
        r,
        "POST",
        &format!("/api/runner/helper/{id}/finish"),
        RUNNER,
        res,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{v}");
    claim
}

async fn no_helper(r: &axum::Router) {
    let (s, v) = call(
        r,
        "POST",
        "/api/runner/helper/claim",
        RUNNER,
        json!({"runner":"local"}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "unexpected job {v}");
}

async fn view(r: &axum::Router, goal: i64) -> Value {
    ok(r, "GET", &format!("/api/goals/{goal}"), OWNER, json!({})).await
}

fn last_message(v: &Value) -> &Value {
    v["messages"]
        .as_array()
        .and_then(|m| m.last())
        .expect("a message")
}

const QUESTIONS: &str = r#"I had a look at the sign-in page.

{"say": "Two quick questions.", "questions": [{"id": "q1", "text": "How should the reset link reach people?", "choices": ["Email", "Text message"], "recommended": 0}]}"#;

const PLAN: &str = r#"```json
{"say": "Here is my plan.", "plan": {"summary": "A forgot-password link on the sign-in page.", "pieces": [{"title": "Reset link", "brief": "Add a reset flow in src/auth.", "done_when": "You can reset your password from the sign-in page."}]}, "remember": ["Tests run with npm test"]}
```"#;

#[tokio::test]
async fn a_goal_becomes_reviewed_work() {
    let (r, _) = hub().await;
    let app = app(&r, false).await;

    // The owner states a goal; the coordinator asks a question.
    let (s, g) = call(
        &r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "Add a password reset\nfor clinic staff"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let goal = g["id"].as_i64().expect("goal id");
    assert_eq!(g["title"], "Add a password reset");
    assert_eq!(g["busy"], true);
    let claim = helper(&r, "coordinator", QUESTIONS, None).await;
    assert_eq!(claim["resume"], false);
    assert_eq!(claim["source"], json!({"kind": "app", "goal_id": goal}));
    let tools: Vec<&str> = claim["disallowed_tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(tools.contains(&"Bash") && tools.contains(&"Edit") && tools.contains(&"Write"));
    let prompt = claim["prompt"].as_str().expect("prompt");
    assert!(prompt.contains("project lead") && prompt.contains("Owner: Add a password reset"));

    let v = view(&r, goal).await;
    assert_eq!(v["goal"]["state"], "talking");
    assert_eq!(v["goal"]["busy"], false);
    let asked = last_message(&v);
    assert_eq!(asked["body"]["kind"], "coordinator");
    let asked_id = asked["id"].as_i64().expect("id");

    // Answers are checked against what was asked.
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/goals/{goal}/answer"),
        OWNER,
        json!({"to": asked_id, "answers": [{"question_id": "q1", "choice": 5}]}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/goals/{goal}/answer"),
        OWNER,
        json!({"to": asked_id, "answers": [{"question_id": "q9", "choice": 0}]}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    ok(&r, "POST", &format!("/api/goals/{goal}/answer"), OWNER, json!({"to": asked_id, "answers": [{"question_id": "q1", "choice": 0, "other": "staff only"}]})).await;
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/goals/{goal}/answer"),
        OWNER,
        json!({"to": asked_id, "answers": [{"question_id": "q1", "choice": 1}]}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "answered twice");

    // The next turn resumes the session with only what is new.
    let claim = helper(&r, "coordinator", PLAN, None).await;
    assert_eq!(claim["resume"], true);
    let prompt = claim["prompt"].as_str().expect("prompt");
    assert!(
        prompt.contains("How should the reset link reach people? -> Email. Also: staff only"),
        "{prompt}"
    );
    assert!(
        !prompt.contains("Owner: Add a password reset"),
        "old messages are not repeated"
    );

    let v = view(&r, goal).await;
    assert_eq!(v["goal"]["state"], "plan_ready");
    assert_eq!(v["drawing"], true);
    let plan_id = last_message(&v)["id"].as_i64().expect("plan id");
    let notes = ok(
        &r,
        "GET",
        &format!("/api/apps/{app}/notes"),
        OWNER,
        json!({}),
    )
    .await;
    assert_eq!(notes[0]["text"], "Tests run with npm test");

    // The designer draws the plan; the page opens in a sandbox.
    let claim = helper(&r, "design", "Page written.", Some("<h1>Plan</h1>")).await;
    assert_eq!(
        claim["source"],
        json!({"kind": "app_copy", "goal_id": goal})
    );
    assert!(claim["prompt"]
        .as_str()
        .expect("prompt")
        .contains("/out/page.html"));
    let v = view(&r, goal).await;
    assert_eq!(v["drawing"], false);
    let page = &v["pages"][0];
    assert_eq!(page["kind"], "plan");
    assert_eq!(page["message_id"], plan_id);
    let resp = raw(
        &r,
        "GET",
        &format!("/api/goals/{goal}/pages/{}", page["id"]),
        OWNER,
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let csp = resp
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        csp.starts_with("sandbox;") && !csp.contains("allow-scripts"),
        "{csp}"
    );
    let (s, _) = call(
        &r,
        "GET",
        &format!("/api/goals/{goal}/pages/{}", page["id"]),
        RUNNER,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "pages are the owner's");

    // Only the latest plan can be approved, and only by the owner.
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/goals/{goal}/approve"),
        OWNER,
        json!({"plan_message_id": plan_id - 1}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/goals/{goal}/approve"),
        RUNNER,
        json!({"plan_message_id": plan_id}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let g = ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/approve"),
        OWNER,
        json!({"plan_message_id": plan_id}),
    )
    .await;
    assert_eq!(g["state"], "working");
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/goals/{goal}/approve"),
        OWNER,
        json!({"plan_message_id": plan_id}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "one approval only");

    // The piece runs as an ordinary thread.
    let work = ok(
        &r,
        "POST",
        "/api/runner/claim",
        RUNNER,
        json!({"runner":"local"}),
    )
    .await;
    let thread = work["thread"]["id"].as_i64().expect("thread");
    assert_eq!(work["thread"]["title"], "Reset link");
    let task = work["thread"]["prompt"].as_str().expect("prompt");
    // Exactly the approved brief: app notes the owner never saw stay out.
    assert!(
        task.contains("Add a reset flow in src/auth.") && !task.contains("Tests run with npm test")
    );
    ok(
        &r,
        "POST",
        &format!("/api/threads/{thread}/events"),
        RUNNER,
        json!({"kind":"message","text":"working"}),
    )
    .await;
    ok(
        &r,
        "POST",
        &format!("/api/threads/{thread}/move"),
        RUNNER,
        json!({"to":"in_review"}),
    )
    .await;

    // The checker sends it back once.
    let v = view(&r, goal).await;
    assert_eq!(v["pieces"][0]["review"], "reviewing");
    let claim = helper(&r, "review", r#"{"pass": false, "summary": "The link is missing on phones.", "issues": ["Show the link on small screens"]}"#, None).await;
    assert_eq!(claim["source"]["kind"], "branch");
    assert_eq!(claim["source"]["thread_id"], thread);
    let v = view(&r, goal).await;
    assert_eq!(v["pieces"][0]["review"], "fixing");
    assert_eq!(v["pieces"][0]["thread_state"], "queued");
    let work = ok(
        &r,
        "POST",
        "/api/runner/claim",
        RUNNER,
        json!({"runner":"local"}),
    )
    .await;
    assert_eq!(work["resume"], true);
    // The checker's notes arrive the way the owner's "do this next" does.
    let notes = &work["messages"][0];
    assert_eq!(notes["kind"], "follow_up");
    assert!(notes["text"]
        .as_str()
        .expect("notes")
        .contains("Show the link on small screens"));
    ok(
        &r,
        "POST",
        &format!("/api/threads/{thread}/messages/delivered"),
        RUNNER,
        json!({"ids": [notes["id"]]}),
    )
    .await;
    ok(
        &r,
        "POST",
        &format!("/api/threads/{thread}/move"),
        RUNNER,
        json!({"to":"in_review"}),
    )
    .await;

    // Round two passes, with screenshots; the coordinator reports.
    let claim = helper(
        &r,
        "review",
        r#"{"pass": true, "summary": "Works on PC and phone."}"#,
        Some("<img alt=after>"),
    )
    .await;
    assert!(claim["prompt"]
        .as_str()
        .expect("prompt")
        .contains("check round 2"));
    let v = view(&r, goal).await;
    assert_eq!(v["pieces"][0]["review"], "passed");
    assert_eq!(v["pieces"][0]["rounds"], 2);
    assert_eq!(v["pages"][1]["kind"], "check");
    assert_eq!(v["pages"][1]["thread_id"], thread);
    assert_eq!(v["goal"]["busy"], true);
    let claim = helper(
        &r,
        "coordinator",
        r#"{"say": "The reset link is built and checked. Try it on your phone."}"#,
        None,
    )
    .await;
    assert!(claim["prompt"]
        .as_str()
        .expect("prompt")
        .contains("Reset link: built and passed"));
    let v = view(&r, goal).await;
    assert_eq!(v["goal"]["state"], "for_review");
    assert_eq!(last_message(&v)["body"]["kind"], "report");
    no_helper(&r).await;

    let g = ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/close"),
        OWNER,
        json!({}),
    )
    .await;
    assert_eq!(g["state"], "done");
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/goals/{goal}/say"),
        OWNER,
        json!({"text": "hello"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_bad_answer_is_asked_again_once_then_the_goal_waits() {
    let (r, _) = hub().await;
    let app = app(&r, false).await;
    let g = ok(
        &r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "Something"}),
    )
    .await;
    let goal = g["id"].as_i64().expect("goal");
    helper(&r, "coordinator", "I forgot the JSON.", None).await;
    let claim = helper(&r, "coordinator", "Still no JSON.", None).await;
    assert!(claim["prompt"]
        .as_str()
        .expect("prompt")
        .contains("could not be used"));
    let v = view(&r, goal).await;
    assert_eq!(v["goal"]["stuck"], true);
    assert_eq!(v["goal"]["busy"], false);
    no_helper(&r).await;
    // Try again starts a fresh session with the whole conversation.
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/retry"),
        OWNER,
        json!({}),
    )
    .await;
    let claim = helper(&r, "coordinator", r#"{"say": "Sorry, here I am."}"#, None).await;
    assert_eq!(claim["resume"], false);
    assert!(claim["prompt"]
        .as_str()
        .expect("prompt")
        .contains("Owner: Something"));
    assert_eq!(view(&r, goal).await["goal"]["stuck"], false);
}

#[tokio::test]
async fn messages_wait_for_the_running_turn_and_restarts_requeue_it() {
    let (r, _) = hub().await;
    let app = app(&r, false).await;
    let g = ok(
        &r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "First"}),
    )
    .await;
    let goal = g["id"].as_i64().expect("goal");
    let first = ok(
        &r,
        "POST",
        "/api/runner/helper/claim",
        RUNNER,
        json!({"runner":"local"}),
    )
    .await;
    // While that turn runs, the owner adds two messages: one more turn, not two.
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/say"),
        OWNER,
        json!({"text": "Second"}),
    )
    .await;
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/say"),
        OWNER,
        json!({"text": "Third"}),
    )
    .await;
    no_helper(&r).await;
    // The runner restarts: the turn already waiting picks up everything, in a
    // fresh session, since the killed one may be half-written.
    ok(
        &r,
        "POST",
        "/api/runner/hello",
        RUNNER,
        json!({"runner":"local"}),
    )
    .await;
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/runner/helper/{}/finish", first["job_id"]),
        RUNNER,
        json!({"ok": true, "text": "{\"say\":\"late\"}"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "a closed job cannot finish");
    let claim = helper(&r, "coordinator", r#"{"say": "Got all three."}"#, None).await;
    let prompt = claim["prompt"].as_str().expect("prompt");
    assert!(
        prompt.contains("Owner: First") && prompt.contains("Owner: Third"),
        "{prompt}"
    );
    assert_eq!(claim["resume"], false);
    no_helper(&r).await;
}

#[tokio::test]
async fn helpers_count_toward_the_spending_cap() {
    let (r, store) = hub().await;
    let app = app(&r, true).await;
    ok(&r, "POST", "/api/spend/cap", OWNER, json!({"cap_usd": 1.0})).await;
    let g = ok(
        &r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "Go"}),
    )
    .await;
    let goal = g["id"].as_i64().expect("goal");
    let claim = ok(
        &r,
        "POST",
        "/api/runner/helper/claim",
        RUNNER,
        json!({"runner":"local"}),
    )
    .await;
    assert_eq!(claim["budget_usd"], 0.9);
    let id = claim["job_id"].as_i64().expect("job");
    ok(
        &r,
        "POST",
        &format!("/api/runner/helper/{id}/started"),
        RUNNER,
        json!({}),
    )
    .await;
    let spend = store.spend().await.expect("spend");
    assert!(
        (spend.pay_per_use_usd - 1.0).abs() < 1e-9,
        "set aside while it runs"
    );
    // It crashes without a cost report: the set-aside money counts.
    ok(
        &r,
        "POST",
        &format!("/api/runner/helper/{id}/finish"),
        RUNNER,
        json!({"ok": false, "text": "container died"}),
    )
    .await;
    let v = view(&r, goal).await;
    assert_eq!(v["goal"]["stuck"], true);
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/retry"),
        OWNER,
        json!({}),
    )
    .await;
    no_helper(&r).await;
    let v = view(&r, goal).await;
    assert_eq!(v["goal"]["stuck"], true);
    assert!(last_message(&v)["body"]["text"]
        .as_str()
        .expect("note")
        .contains("limit"));
}

#[tokio::test]
async fn stopping_a_goal_cancels_waiting_work() {
    let (r, _) = hub().await;
    let app = app(&r, false).await;
    let g = ok(
        &r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "Two things"}),
    )
    .await;
    let goal = g["id"].as_i64().expect("goal");
    helper(&r, "coordinator", r#"{"say":"Plan.","plan":{"summary":"Two parts.","pieces":[{"title":"A","brief":"a","done_when":"a works"},{"title":"B","brief":"b","done_when":"b works"}]}}"#, None).await;
    let v = view(&r, goal).await;
    let plan_id = last_message(&v)["id"].as_i64().expect("plan");
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/approve"),
        OWNER,
        json!({"plan_message_id": plan_id}),
    )
    .await;
    let g = ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/stop"),
        OWNER,
        json!({}),
    )
    .await;
    assert_eq!(g["state"], "stopped");
    let threads = ok(&r, "GET", "/api/threads", OWNER, json!({})).await;
    assert!(threads
        .as_array()
        .expect("threads")
        .iter()
        .all(|t| t["state"] == "failed"));
    // The design job for the plan was cancelled too.
    no_helper(&r).await;
}

#[tokio::test]
async fn helpers_wait_while_their_max_account_is_out() {
    let (r, store) = hub().await;
    let app = app(&r, false).await;
    ok(
        &r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "Go"}),
    )
    .await;
    sqlx::query(
        "INSERT INTO profile_limits (profile, until) VALUES ('A', strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+1 hour'))",
    )
    .execute(store.pool())
    .await
    .expect("limit");
    no_helper(&r).await;
    sqlx::query("DELETE FROM profile_limits")
        .execute(store.pool())
        .await
        .expect("clear");
    helper(&r, "coordinator", r#"{"say": "Back."}"#, None).await;
}

/// One approved piece whose agent finished, so its check is waiting.
async fn approved_one_piece(r: &axum::Router) -> (i64, i64) {
    let app = app(r, false).await;
    let g = ok(
        r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "Go"}),
    )
    .await;
    let goal = g["id"].as_i64().expect("goal");
    helper(r, "coordinator", PLAN, None).await;
    let v = view(r, goal).await;
    let plan_id = last_message(&v)["id"].as_i64().expect("plan");
    ok(
        r,
        "POST",
        &format!("/api/goals/{goal}/approve"),
        OWNER,
        json!({"plan_message_id": plan_id}),
    )
    .await;
    helper(r, "design", "done", Some("<h1>x</h1>")).await;
    let work = ok(
        r,
        "POST",
        "/api/runner/claim",
        RUNNER,
        json!({"runner":"local"}),
    )
    .await;
    let thread = work["thread"]["id"].as_i64().expect("thread");
    ok(
        r,
        "POST",
        &format!("/api/threads/{thread}/events"),
        RUNNER,
        json!({"kind":"message","text":"w"}),
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
    (goal, thread)
}

#[tokio::test]
async fn big_pages_fit_through() {
    let (r, _) = hub().await;
    let (goal, _) = approved_one_piece(&r).await;
    let page = format!("<p>{}</p>", "a".repeat(3 << 20));
    helper(
        &r,
        "review",
        r#"{"pass": true, "summary": "Works."}"#,
        Some(&page),
    )
    .await;
    let v = view(&r, goal).await;
    assert_eq!(v["pieces"][0]["review"], "passed");
    assert_eq!(v["pages"].as_array().map(Vec::len), Some(2));
}

#[tokio::test]
async fn what_the_owner_says_during_a_retry_reaches_the_lead() {
    let (r, _) = hub().await;
    let app = app(&r, false).await;
    let g = ok(
        &r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "First"}),
    )
    .await;
    let goal = g["id"].as_i64().expect("goal");
    helper(&r, "coordinator", "no json here", None).await;
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/say"),
        OWNER,
        json!({"text": "Leave billing alone"}),
    )
    .await;
    let claim = helper(&r, "coordinator", r#"{"say": "Fixed."}"#, None).await;
    let prompt = claim["prompt"].as_str().expect("prompt");
    assert!(
        prompt.contains("could not be used") && prompt.contains("Leave billing alone"),
        "{prompt}"
    );
    no_helper(&r).await;
}

#[tokio::test]
async fn talking_after_a_failed_write_up_writes_it_again() {
    let (r, _) = hub().await;
    let (goal, _) = approved_one_piece(&r).await;
    helper(&r, "review", r#"{"pass": true, "summary": "Works."}"#, None).await;
    let claim = ok(
        &r,
        "POST",
        "/api/runner/helper/claim",
        RUNNER,
        json!({"runner":"local"}),
    )
    .await;
    let id = claim["job_id"].as_i64().expect("job");
    ok(
        &r,
        "POST",
        &format!("/api/runner/helper/{id}/finish"),
        RUNNER,
        json!({"ok": false, "text": "boom"}),
    )
    .await;
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/say"),
        OWNER,
        json!({"text": "How is it going?"}),
    )
    .await;
    helper(&r, "coordinator", r#"{"say": "All good."}"#, None).await;
    helper(
        &r,
        "coordinator",
        r#"{"say": "Here is what was done."}"#,
        None,
    )
    .await;
    assert_eq!(view(&r, goal).await["goal"]["state"], "for_review");
}

#[tokio::test]
async fn a_restart_does_not_rerun_checks_for_a_stopped_goal() {
    let (r, _) = hub().await;
    let (goal, _) = approved_one_piece(&r).await;
    let claim = ok(
        &r,
        "POST",
        "/api/runner/helper/claim",
        RUNNER,
        json!({"runner":"local"}),
    )
    .await;
    assert_eq!(claim["kind"], "review");
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/stop"),
        OWNER,
        json!({}),
    )
    .await;
    ok(
        &r,
        "POST",
        "/api/runner/hello",
        RUNNER,
        json!({"runner":"local"}),
    )
    .await;
    no_helper(&r).await;
}

#[tokio::test]
async fn sending_a_piece_back_during_its_check_checks_the_new_code() {
    let (r, _) = hub().await;
    let (goal, thread) = approved_one_piece(&r).await;
    // The owner sends it back while its check waits: that check is dropped.
    ok(
        &r,
        "POST",
        &format!("/api/threads/{thread}/move"),
        OWNER,
        json!({"to":"queued"}),
    )
    .await;
    no_helper(&r).await;
    assert_eq!(view(&r, goal).await["pieces"][0]["review"], "building");
    let work = ok(
        &r,
        "POST",
        "/api/runner/claim",
        RUNNER,
        json!({"runner":"local"}),
    )
    .await;
    assert_eq!(work["thread"]["id"], thread);
    ok(
        &r,
        "POST",
        &format!("/api/threads/{thread}/move"),
        RUNNER,
        json!({"to":"in_review"}),
    )
    .await;
    helper(&r, "review", r#"{"pass": true, "summary": "Works."}"#, None).await;
    helper(&r, "coordinator", r#"{"say": "Done."}"#, None).await;
    assert_eq!(view(&r, goal).await["goal"]["state"], "for_review");
}

#[tokio::test]
async fn a_check_missed_in_a_crash_is_queued_again() {
    let (r, store) = hub().await;
    let (goal, thread) = approved_one_piece(&r).await;
    // As if the hub stopped right after the thread reached review.
    sqlx::query("DELETE FROM helper_jobs WHERE kind = 'review'")
        .execute(store.pool())
        .await
        .expect("delete");
    sqlx::query("UPDATE goal_pieces SET review = 'building' WHERE thread_id = ?")
        .bind(thread)
        .execute(store.pool())
        .await
        .expect("update");
    helper(&r, "review", r#"{"pass": true, "summary": "Works."}"#, None).await;
    assert_eq!(view(&r, goal).await["pieces"][0]["review"], "passed");
}

/// Claims the next helper job, whatever it is, without finishing it.
async fn claim_any(r: &axum::Router) -> Value {
    let claim = ok(
        r,
        "POST",
        "/api/runner/helper/claim",
        RUNNER,
        json!({"runner":"local"}),
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

#[tokio::test]
async fn a_failed_piece_tried_again_is_checked_again() {
    let (r, _) = hub().await;
    let (goal, thread) = approved_one_piece(&r).await;
    let check = claim_any(&r).await;
    assert_eq!(check["kind"], "review");
    // The owner gives up on it, then tries it again.
    ok(
        &r,
        "POST",
        &format!("/api/threads/{thread}/move"),
        OWNER,
        json!({"to":"failed"}),
    )
    .await;
    finish(&r, &check, r#"{"pass": true, "summary": "Works."}"#).await;
    helper(
        &r,
        "coordinator",
        r#"{"say": "It could not be finished."}"#,
        None,
    )
    .await;
    assert_eq!(view(&r, goal).await["pieces"][0]["review"], "failed");
    ok(
        &r,
        "POST",
        &format!("/api/threads/{thread}/move"),
        OWNER,
        json!({"to":"queued"}),
    )
    .await;
    assert_eq!(view(&r, goal).await["pieces"][0]["review"], "building");
}

#[tokio::test]
async fn a_talk_turn_retry_does_not_become_the_report() {
    let (r, _) = hub().await;
    let (goal, _) = approved_one_piece(&r).await;
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/say"),
        OWNER,
        json!({"text": "Is the menu blue?"}),
    )
    .await;
    let check = claim_any(&r).await;
    assert_eq!(check["kind"], "review");
    let talk = claim_any(&r).await;
    assert_eq!(talk["kind"], "coordinator");
    finish(&r, &check, r#"{"pass": true, "summary": "Works."}"#).await;
    finish(&r, &talk, "no json").await;
    let report = claim_any(&r).await;
    assert!(report["prompt"]
        .as_str()
        .is_some_and(|p| !p.contains("could not be used")));
    finish(&r, &report, r#"{"say": "Here is what was done."}"#).await;
    let retry = claim_any(&r).await;
    assert!(
        retry["prompt"]
            .as_str()
            .is_some_and(|p| p.contains("could not be used")),
        "{retry}"
    );
}

#[tokio::test]
async fn a_restart_keeps_a_talk_turn_when_a_report_waits() {
    let (r, _) = hub().await;
    let (goal, _) = approved_one_piece(&r).await;
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/say"),
        OWNER,
        json!({"text": "Is the menu blue?"}),
    )
    .await;
    let check = claim_any(&r).await;
    let talk = claim_any(&r).await;
    assert_eq!(talk["kind"], "coordinator");
    finish(&r, &check, r#"{"pass": true, "summary": "Works."}"#).await;
    ok(
        &r,
        "POST",
        "/api/runner/hello",
        RUNNER,
        json!({"runner":"local"}),
    )
    .await;
    let first = claim_any(&r).await;
    finish(&r, &first, r#"{"say": "Here is what was done."}"#).await;
    let second = claim_any(&r).await;
    assert_eq!(second["kind"], "coordinator");
    finish(&r, &second, r#"{"say": "Yes, it is blue."}"#).await;
    no_helper(&r).await;
}
