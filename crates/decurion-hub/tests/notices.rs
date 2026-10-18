//! Phone notices: each names its app, and the one tap a notice offers
//! (Approve plan, Accept) goes through the owner's own routes with the
//! phone's sign-in cookie and the notice's receipt, acting once.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::protected::Hold;
use decurion_hub::push::{self, Act};
use decurion_hub::{router, store::Store, AppState, Keys};
use decurion_proto::goal::HelperResult;
use decurion_proto::ThreadState;
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

/// A call with the owner key, the runner key, or a phone's cookie as the
/// service worker sends it (`sec-fetch-site` given).
async fn call(
    r: &axum::Router,
    path: &str,
    key: Option<&str>,
    cookie: Option<(&str, &str)>,
    body: Value,
) -> (StatusCode, Value) {
    let mut b = Request::post(path).header(header::CONTENT_TYPE, "application/json");
    if let Some(k) = key {
        b = b.header(header::AUTHORIZATION, format!("Bearer {k}"));
    }
    if let Some((c, site)) = cookie {
        b = b
            .header(header::COOKIE, format!("decurion_device={c}"))
            .header("sec-fetch-site", site);
    }
    let resp = r
        .clone()
        .oneshot(b.body(Body::from(body.to_string())).expect("request"))
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

async fn app(r: &axum::Router) -> i64 {
    let (s, v) = call(
        r,
        "/api/apps",
        Some(OWNER),
        None,
        json!({"name":"refill-tracker","repo_path":"C:\\repos\\refill","profile":"A"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    v["id"].as_i64().expect("app id")
}

/// Runs the project lead's next turn with `text` as its answer and returns
/// the notice it gave.
async fn lead_says(r: &axum::Router, store: &Store, text: &str) -> Option<push::Notice> {
    let (s, claim) = call(
        r,
        "/api/runner/helper/claim",
        Some(RUNNER),
        None,
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{claim}");
    let job = claim["job_id"].as_i64().expect("job id");
    store.helper_started(job).await.expect("started");
    let res = HelperResult {
        ok: true,
        text: text.into(),
        cost_usd: Some(0.01),
        input_tokens: None,
        output_tokens: None,
        page_html: None,
    };
    store
        .finish_helper(job, &res, &Hold::Clear)
        .await
        .expect("finished")
}

const QUESTION: &str = r#"{"say": "One question.", "questions": [{"id": "q1", "text": "Text or email?", "choices": ["Text", "Email"], "recommended": 0}]}"#;
const PLAN: &str = r#"{"say": "Here is my plan.", "plan": {"summary": "Reminders.", "pieces": [{"title": "Reminder screen", "brief": "Add it.", "done_when": "It shows."}]}}"#;

#[tokio::test]
async fn a_plan_notice_names_the_app_and_its_approve_tap_works() {
    let (r, store) = hub().await;
    let app = app(&r).await;
    let (s, g) = call(
        &r,
        "/api/goals",
        Some(OWNER),
        None,
        json!({"app_id": app, "text": "Add refill reminders"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let goal = g["id"].as_i64().expect("goal");

    let n = lead_says(&r, &store, QUESTION).await.expect("a notice");
    assert_eq!(n.title, "refill-tracker: your project lead has a question");
    assert_eq!(n.body, "Add refill reminders");
    assert_eq!(n.url, format!("/goals#goal-{goal}"));
    assert_eq!(n.act, None);

    call(
        &r,
        &format!("/api/goals/{goal}/say"),
        Some(OWNER),
        None,
        json!({"text": "Text"}),
    )
    .await;
    let n = lead_says(&r, &store, PLAN).await.expect("a notice");
    assert_eq!(n.title, "refill-tracker: plan ready to approve");
    let Some(Act::Approve { goal: g, plan }) = n.act else {
        panic!("no Approve on {n:?}");
    };
    assert_eq!(g, goal);
    let view = store.goal_view(goal).await.expect("view");
    let view = serde_json::to_value(view).expect("json");
    let latest = view["messages"]
        .as_array()
        .and_then(|m| m.iter().rev().find(|m| m["body"]["kind"] == "coordinator"))
        .and_then(|m| m["id"].as_i64());
    assert_eq!(Some(plan), latest);

    // The phone's tap: its cookie, the plan it was told about.
    let (_, phone) = store.add_device("Android phone").await.expect("device");
    let (s, v) = call(
        &r,
        &format!("/api/goals/{goal}/approve"),
        None,
        Some((&phone, "same-origin")),
        json!({"plan_message_id": plan, "receipt": "notice-receipt-1"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "working");
    // A second tap changes nothing and says why in plain words.
    let (s, v) = call(
        &r,
        &format!("/api/goals/{goal}/approve"),
        None,
        Some((&phone, "same-origin")),
        json!({"plan_message_id": plan, "receipt": "notice-receipt-1"}),
    )
    .await;
    assert!(s.is_client_error(), "{s} {v}");
}

#[tokio::test]
async fn accept_from_a_notice_acts_once_and_only_with_the_phones_cookie() {
    let (r, store) = hub().await;
    let app = app(&r).await;
    let (_, t) = call(
        &r,
        "/api/threads",
        Some(OWNER),
        None,
        json!({"app_id": app, "title": "Reminder screen", "prompt": "do it"}),
    )
    .await;
    let id = t["id"].as_i64().expect("id");
    let (s, _) = call(
        &r,
        "/api/runner/claim",
        Some(RUNNER),
        None,
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = call(
        &r,
        &format!("/api/threads/{id}/move"),
        Some(RUNNER),
        None,
        json!({"to": "in_review"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // What the phone is told when the agent finishes.
    let t = store.thread(id).await.expect("thread");
    let n = push::for_move(&t, "refill-tracker").expect("a notice");
    assert_eq!(n.title, "refill-tracker: ready for review");
    assert_eq!(n.act, Some(Act::Accept { thread: id }));
    let mut running = t.clone();
    running.state = ThreadState::Running;
    assert!(push::for_move(&running, "refill-tracker").is_none());

    let tap = json!({"to": "done", "expect": "in_review", "receipt": "notice-receipt-2"});
    let path = format!("/api/threads/{id}/move");
    let (_, phone) = store.add_device("Android phone").await.expect("device");
    // No cookie, a signed-out one, or a tap from another site does nothing.
    assert_eq!(
        call(&r, &path, None, None, tap.clone()).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &r,
            &path,
            None,
            Some(("not-a-device", "same-origin")),
            tap.clone()
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&r, &path, None, Some((&phone, "cross-site")), tap.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        store.thread(id).await.expect("thread").state,
        ThreadState::InReview
    );

    // Two phones tap the same notice: one accept, the same answer twice.
    let (_, other) = store.add_device("Tablet").await.expect("device");
    let (s, first) = call(&r, &path, None, Some((&phone, "same-origin")), tap.clone()).await;
    assert_eq!(s, StatusCode::OK, "{first}");
    let (s, again) = call(&r, &path, None, Some((&other, "same-origin")), tap).await;
    assert_eq!(s, StatusCode::OK, "{again}");
    assert_eq!(first["state"], "done");
    assert_eq!(again["state"], "done");
    let moves: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit WHERE thread_id = ? AND to_state = 'done'")
            .bind(id)
            .fetch_one(store.pool())
            .await
            .expect("count");
    assert_eq!(moves, 1);
}
