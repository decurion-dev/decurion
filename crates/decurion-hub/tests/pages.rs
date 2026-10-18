//! Questions inside pages agents drew: the designer and the checker ask, the
//! hub marks where in the page, and the owner answers beside it. The runner's
//! side is played by the test.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::{router, store::Store, AppState, Keys};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";

async fn hub() -> axum::Router {
    let store = Store::open("sqlite::memory:").await.expect("open store");
    router(AppState::new(
        store,
        Keys {
            owner: OWNER.into(),
            runner: RUNNER.into(),
        },
        "abc123".into(),
        None,
    ))
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

/// Claims the next helper job, checks its kind, and finishes it with `text`.
async fn helper(r: &axum::Router, kind: &str, text: &str, page: Option<&str>) -> Value {
    let (s, claim) = call(
        r,
        "POST",
        "/api/runner/helper/claim",
        RUNNER,
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
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

const PLAN: &str = r#"{"say": "Here is my plan.", "plan": {"summary": "A forgot-password link on the sign-in page.", "pieces": [{"title": "Reset link", "brief": "Add a reset flow in src/auth.", "done_when": "You can reset your password from the sign-in page."}]}}"#;

const PAGE: &str = r#"<html><head><title>
  Reset &amp; sign-in </title></head><body><h1>Plan</h1>
<div data-question="q1"></div>
<div data-question = 'q2' >  </div>
<div data-question="zz"></div>
</body></html>"#;

const ASKS: &str = r#"Page written.

{"questions": [{"id": "q1", "text": "Show the link <script>alert(1)</script> on phones?", "choices": ["Yes", "No"], "recommended": 0}, {"id": "q2", "text": "Anything to add?", "choices": []}]}"#;

/// A goal with a plan whose design job is waiting.
async fn planned_goal(r: &axum::Router) -> i64 {
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
    let goal = ok(
        r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "Add a password reset"}),
    )
    .await["id"]
        .as_i64()
        .expect("goal id");
    helper(r, "coordinator", PLAN, None).await;
    goal
}

async fn library(r: &axum::Router, goal: i64) -> Value {
    ok(
        r,
        "GET",
        &format!("/api/library?goal={goal}"),
        OWNER,
        json!({}),
    )
    .await
}

#[tokio::test]
async fn a_designer_asks_in_its_page_and_the_owner_answers_beside_it() {
    let r = hub().await;
    let goal = planned_goal(&r).await;
    helper(&r, "design", ASKS, Some(PAGE)).await;

    // The library names the page by its own title and shows it waits.
    let lib = library(&r, goal).await;
    let page = lib[0]["id"].as_i64().expect("page id");
    assert_eq!(lib[0]["title"], "Reset & sign-in");
    assert_eq!(lib[0]["questions"], 2);
    assert_eq!(lib[0]["waiting"], true);

    let base = format!("/api/goals/{goal}/pages/{page}");
    let qs = ok(&r, "GET", &format!("{base}/questions"), OWNER, json!({})).await;
    assert_eq!(qs["page"]["id"], page);
    assert_eq!(qs["questions"][0]["choices"], json!(["Yes", "No"]));
    assert_eq!(qs["questions"][1]["choices"], json!([]));
    assert!(qs.get("answers").is_none() && qs.get("answered_at").is_none());

    // The page shows the hub's own note where each question belongs.
    let resp = raw(&r, "GET", &base, OWNER, json!({})).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let csp = resp
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    assert!(csp.starts_with("sandbox;"), "{csp}");
    assert!(!csp.contains("allow-scripts"), "{csp}");
    assert!(csp.contains("form-action 'none'"), "{csp}");
    assert!(csp.contains("frame-ancestors 'self'"), "{csp}");
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let html = String::from_utf8_lossy(&bytes);
    assert!(html.contains(
        "Question 1: Show the link &lt;script&gt;alert(1)&lt;/script&gt; on phones? · Answer it beside this page."
    ), "{html}");
    assert!(html.contains("Question 2: Anything to add? · Answer it beside this page."));
    assert!(!html.contains("<script>"), "{html}");
    assert!(!html.contains("data-question=\"q1\""));
    assert!(
        html.contains("<div data-question=\"zz\"></div>"),
        "unknown markers stay"
    );

    // Answers are checked against what was asked, in plain words.
    let answer = |answers: Value, receipt: &str| json!({"answers": answers, "receipt": receipt});
    let (s, why) = call(
        &r,
        "POST",
        &format!("{base}/answer"),
        OWNER,
        answer(json!([{"question_id": "q1", "choice": 2}]), "receipt-bad-1"),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        why,
        "That is not one of the choices. Pick one of the choices shown."
    );
    let (s, why) = call(
        &r,
        "POST",
        &format!("{base}/answer"),
        OWNER,
        answer(json!([]), "receipt-bad-2"),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(why, "Answer at least one question.");
    let (s, why) = call(
        &r,
        "POST",
        &format!("{base}/answer"),
        OWNER,
        answer(
            json!([{"question_id": "q2", "other": "   "}]),
            "receipt-bad-3",
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        why,
        "This question needs an answer in words. Write a few words, then send."
    );

    // The runner key reaches neither route.
    let (s, _) = call(&r, "GET", &format!("{base}/questions"), RUNNER, json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &r,
        "POST",
        &format!("{base}/answer"),
        RUNNER,
        answer(
            json!([{"question_id": "q1", "choice": 0}]),
            "receipt-runner",
        ),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // A good answer is recorded and reaches the project lead.
    let good = answer(
        json!([
            {"question_id": "q1", "choice": 0, "other": " bigger please "},
            {"question_id": "q2", "other": "Keep it calm"}
        ]),
        "receipt-good",
    );
    let first = ok(&r, "POST", &format!("{base}/answer"), OWNER, good.clone()).await;
    assert_eq!(first["answers"][0]["other"], "bigger please");
    assert_eq!(first["answers"][1]["question_id"], "q2");
    assert!(first["answered_at"].is_string());
    assert_eq!(first["page"]["waiting"], false);
    let again = ok(&r, "POST", &format!("{base}/answer"), OWNER, good).await;
    assert_eq!(again, first, "a resent tap gets the first result");
    let (s, why) = call(
        &r,
        "POST",
        &format!("{base}/answer"),
        OWNER,
        answer(
            json!([{"question_id": "q1", "choice": 1}]),
            "receipt-second",
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(why, "those questions are already answered");
    assert_eq!(library(&r, goal).await[0]["waiting"], false);

    let v = ok(&r, "GET", &format!("/api/goals/{goal}"), OWNER, json!({})).await;
    assert_eq!(v["goal"]["busy"], true, "the lead's turn is queued");
    let said = v["messages"]
        .as_array()
        .and_then(|m| m.last())
        .expect("a message");
    assert_eq!(said["body"]["kind"], "owner");
    let text = "My answers on the page \"Reset & sign-in\":\n\
- Show the link <script>alert(1)</script> on phones? → Yes. Also: bigger please\n\
- Anything to add? → Keep it calm";
    assert_eq!(said["body"]["text"], text);
    let claim = helper(&r, "coordinator", r#"{"say": "Thanks."}"#, None).await;
    assert!(
        claim["prompt"].as_str().expect("prompt").contains(text),
        "{claim}"
    );
}

#[tokio::test]
async fn a_closed_goal_takes_no_answers() {
    let r = hub().await;
    let goal = planned_goal(&r).await;
    helper(&r, "design", ASKS, Some(PAGE)).await;
    let page = library(&r, goal).await[0]["id"].as_i64().expect("page id");
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/stop"),
        OWNER,
        json!({}),
    )
    .await;
    assert_eq!(library(&r, goal).await[0]["waiting"], false);
    let (s, why) = call(
        &r,
        "POST",
        &format!("/api/goals/{goal}/pages/{page}/answer"),
        OWNER,
        json!({"answers": [{"question_id": "q1", "choice": 0}]}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(
        why,
        "This goal is closed, so its questions can no longer be answered."
    );
}

#[tokio::test]
async fn broken_questions_never_cost_the_owner_a_page() {
    let r = hub().await;
    let goal = planned_goal(&r).await;
    // One choice is not a choice: the questions go, the page stays.
    helper(
        &r,
        "design",
        r#"Done. {"questions": [{"id": "q1", "text": "Keep it?", "choices": ["Only"]}]}"#,
        Some("<h1>Plan</h1><div data-question=\"q1\"></div>"),
    )
    .await;
    let lib = library(&r, goal).await;
    assert_eq!(lib.as_array().map(Vec::len), Some(1));
    assert_eq!(lib[0]["title"], "Visual plan", "no title of its own");
    assert_eq!(lib[0]["questions"], 0);
    assert_eq!(lib[0]["waiting"], false);
    let page = lib[0]["id"].as_i64().expect("page id");
    let qs = ok(
        &r,
        "GET",
        &format!("/api/goals/{goal}/pages/{page}/questions"),
        OWNER,
        json!({}),
    )
    .await;
    assert_eq!(qs["questions"], json!([]));
    let resp = raw(
        &r,
        "GET",
        &format!("/api/goals/{goal}/pages/{page}"),
        OWNER,
        json!({}),
    )
    .await;
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    assert!(String::from_utf8_lossy(&bytes).contains("<div data-question=\"q1\"></div>"));
    let (s, why) = call(
        &r,
        "POST",
        &format!("/api/goals/{goal}/pages/{page}/answer"),
        OWNER,
        json!({"answers": [{"question_id": "q1", "choice": 0}]}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(why, "This page asks no questions.");
}

#[tokio::test]
async fn a_checker_asks_about_what_it_built() {
    let r = hub().await;
    let goal = planned_goal(&r).await;
    helper(&r, "design", "Page written.", Some("<h1>Plan</h1>")).await;
    let v = ok(&r, "GET", &format!("/api/goals/{goal}"), OWNER, json!({})).await;
    let plan_id = v["messages"]
        .as_array()
        .and_then(|m| m.iter().rev().find(|m| m["body"]["kind"] == "coordinator"))
        .and_then(|m| m["id"].as_i64())
        .expect("plan id");
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/approve"),
        OWNER,
        json!({"plan_message_id": plan_id}),
    )
    .await;
    let work = ok(
        &r,
        "POST",
        "/api/runner/claim",
        RUNNER,
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    let thread = work["thread"]["id"].as_i64().expect("thread");
    ok(
        &r,
        "POST",
        &format!("/api/threads/{thread}/move"),
        RUNNER,
        json!({"to":"in_review"}),
    )
    .await;
    // The checker writes up what it found and asks the owner one thing.
    helper(
        &r,
        "review",
        r#"{"pass": true, "summary": "Works on PC and phone.", "questions": [{"id": "keep", "text": "Keep the old link too?", "choices": ["Yes", "No"]}]}"#,
        Some("<title>As built: reset link</title><p>Found two old links.</p><div data-question=\"keep\"></div>"),
    )
    .await;
    let v = ok(&r, "GET", &format!("/api/goals/{goal}"), OWNER, json!({})).await;
    assert_eq!(v["pieces"][0]["review"], "passed");
    let check = v["pages"]
        .as_array()
        .and_then(|p| p.iter().find(|p| p["kind"] == "check"))
        .and_then(|p| p["id"].as_i64())
        .expect("check page");
    let qs = ok(
        &r,
        "GET",
        &format!("/api/goals/{goal}/pages/{check}/questions"),
        OWNER,
        json!({}),
    )
    .await;
    assert_eq!(qs["page"]["title"], "As built: reset link");
    assert_eq!(qs["page"]["thread_id"], thread);
    assert_eq!(qs["questions"][0]["id"], "keep");
    assert_eq!(qs["page"]["waiting"], true);
    let resp = raw(
        &r,
        "GET",
        &format!("/api/goals/{goal}/pages/{check}"),
        OWNER,
        json!({}),
    )
    .await;
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    assert!(String::from_utf8_lossy(&bytes).contains("Question 1: Keep the old link too?"));
}

#[tokio::test]
async fn a_check_with_broken_questions_still_counts() {
    let r = hub().await;
    let goal = planned_goal(&r).await;
    helper(&r, "design", "Page written.", None).await;
    let v = ok(&r, "GET", &format!("/api/goals/{goal}"), OWNER, json!({})).await;
    let plan_id = v["messages"]
        .as_array()
        .and_then(|m| m.iter().rev().find(|m| m["body"]["kind"] == "coordinator"))
        .and_then(|m| m["id"].as_i64())
        .expect("plan id");
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/approve"),
        OWNER,
        json!({"plan_message_id": plan_id}),
    )
    .await;
    let work = ok(
        &r,
        "POST",
        "/api/runner/claim",
        RUNNER,
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    let thread = work["thread"]["id"].as_i64().expect("thread");
    ok(
        &r,
        "POST",
        &format!("/api/threads/{thread}/move"),
        RUNNER,
        json!({"to":"in_review"}),
    )
    .await;
    helper(
        &r,
        "review",
        r#"{"pass": true, "summary": "Works.", "questions": [{"id": "q 1", "text": "?", "choices": ["Only"]}]}"#,
        Some("<p>As built</p>"),
    )
    .await;
    let v = ok(&r, "GET", &format!("/api/goals/{goal}"), OWNER, json!({})).await;
    assert_eq!(v["pieces"][0]["review"], "passed", "{v}");
    assert_eq!(library(&r, goal).await[0]["questions"], 0);
}
