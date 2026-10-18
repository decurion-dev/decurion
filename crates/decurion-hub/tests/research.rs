//! Research pieces from plan to report, and later pieces of a goal hearing
//! what earlier ones found, with the runner's side played by the test.

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

/// The page at `path` as the owner gets it: status, CSP header and body.
async fn page(r: &axum::Router, path: &str) -> (StatusCode, String, String) {
    let resp = raw(r, "GET", path, OWNER, json!({})).await;
    let status = resp.status();
    let csp = resp
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, csp, String::from_utf8_lossy(&bytes).into_owned())
}

async fn app(r: &axum::Router) -> i64 {
    let v = ok(
        r,
        "POST",
        "/api/apps",
        OWNER,
        json!({"name":"portal","repo_path":"C:\\repos\\portal","profile":"A","pay_per_use":false}),
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

async fn view(r: &axum::Router, goal: i64) -> Value {
    ok(r, "GET", &format!("/api/goals/{goal}"), OWNER, json!({})).await
}

/// Starts a goal, has the coordinator answer with `plan`, draws it and
/// approves it. Returns the goal's id.
async fn approved(r: &axum::Router, app: i64, plan: &str) -> i64 {
    let g = ok(
        r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "How do staff sign in today?"}),
    )
    .await;
    let goal = g["id"].as_i64().expect("goal id");
    let claim = helper(r, "coordinator", plan, None).await;
    let prompt = claim["prompt"].as_str().expect("prompt");
    // The project lead knows research pieces exist and how to plan one.
    assert!(prompt.contains("\"research\": true"), "{prompt}");
    helper(r, "design", "Page written.", Some("<h1>Plan</h1>")).await;
    let v = view(r, goal).await;
    assert_eq!(v["goal"]["state"], "plan_ready");
    let plan_id = v["messages"]
        .as_array()
        .and_then(|m| m.iter().rev().find(|m| m["body"]["kind"] == "coordinator"))
        .and_then(|m| m["id"].as_i64())
        .expect("plan message");
    let g = ok(
        r,
        "POST",
        &format!("/api/goals/{goal}/approve"),
        OWNER,
        json!({"plan_message_id": plan_id}),
    )
    .await;
    assert_eq!(g["state"], "working");
    goal
}

async fn claim(r: &axum::Router) -> Value {
    ok(
        r,
        "POST",
        "/api/runner/claim",
        RUNNER,
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await
}

async fn review_jobs(store: &Store) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM helper_jobs WHERE kind = 'review'")
        .fetch_one(store.pool())
        .await
        .expect("count review jobs")
}

const RESEARCH_PLAN: &str = r#"{"say": "I will find out.", "plan": {"summary": "Find out how sign-in works.", "pieces": [{"title": "How sign-in works", "brief": "Read src/auth and find out how staff sign in.", "done_when": "You know how staff sign in.", "research": true}]}}"#;

const SUMMARY: &str = "Staff sign in with <email> & a code; \"remember me\" lasts 30 days.";

#[tokio::test]
async fn a_research_piece_ends_in_a_report_and_never_a_check() {
    let (r, store) = hub().await;
    let app = app(&r).await;
    let goal = approved(&r, app, RESEARCH_PLAN).await;

    // The piece runs as a research thread, told to find out, not build.
    let work = claim(&r).await;
    let thread = work["thread"]["id"].as_i64().expect("thread");
    assert_eq!(work["thread"]["research"], true, "{work}");
    let task = work["thread"]["prompt"].as_str().expect("prompt");
    assert!(
        task.contains("This is a research piece") && task.contains("not to change the app"),
        "{task}"
    );
    ok(
        &r,
        "POST",
        &format!("/api/threads/{thread}/events"),
        RUNNER,
        json!({"kind":"finished","ok":true,"summary":SUMMARY}),
    )
    .await;

    // Without a page of its own, the hub makes one from the agent's summary,
    // escaped.
    let report = format!("/api/runner/threads/{thread}/report");
    let (s, v) = call(&r, "POST", &report, RUNNER, json!({})).await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{v}");
    let path = format!("/api/goals/{goal}/reports/{thread}");
    let (s, csp, html) = page(&r, &path).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        csp.starts_with("sandbox;") && !csp.contains("allow-scripts"),
        "{csp}"
    );
    assert!(html.contains("<title>How sign-in works</title>"), "{html}");
    assert!(html.contains("did not draw a report page"), "{html}");
    assert!(
        html.contains("Staff sign in with &lt;email&gt; &amp; a code; &quot;remember me&quot;"),
        "{html}"
    );
    assert!(
        !html.contains("<email>") && !html.contains("<script"),
        "{html}"
    );
    // A blank page counts as none.
    let (s, _) = call(&r, "POST", &report, RUNNER, json!({"html": "  \n"})).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(page(&r, &path)
        .await
        .2
        .contains("did not draw a report page"));

    // The agent's own page replaces it.
    let (s, _) = call(
        &r,
        "POST",
        &report,
        RUNNER,
        json!({"html": "<h1>Findings</h1>"}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    ok(
        &r,
        "POST",
        &format!("/api/threads/{thread}/move"),
        RUNNER,
        json!({"to":"in_review"}),
    )
    .await;

    // No checker: the piece is settled with its report.
    let v = view(&r, goal).await;
    let piece = &v["pieces"][0];
    assert_eq!(piece["research"], true, "{piece}");
    assert_eq!(piece["review"], "passed", "{piece}");
    assert_eq!(piece["report"], true, "{piece}");
    assert!(piece["review_summary"].is_null(), "{piece}");
    assert_eq!(piece["agent_summary"], SUMMARY, "{piece}");
    assert!(
        v["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .any(|m| m["body"]["text"]
                == "How sign-in works: the research is done. Read the report."),
        "{v}"
    );
    assert_eq!(review_jobs(&store).await, 0, "research is never checked");
    // Every piece is settled, so the project lead writes up the goal next.
    let claim = helper(
        &r,
        "coordinator",
        r#"{"say": "Here is what we found."}"#,
        None,
    )
    .await;
    assert!(
        claim["prompt"]
            .as_str()
            .expect("prompt")
            .contains("All the pieces of work for this goal are finished"),
        "{claim}"
    );
    assert_eq!(review_jobs(&store).await, 0);

    // The report is the owner's, in the same sandbox as the plan pages.
    let (s, report_csp, html) = page(&r, &path).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(html, "<h1>Findings</h1>");
    let plan_page = v["pages"][0]["id"].as_i64().expect("plan page");
    let (_, plan_csp, _) = page(&r, &format!("/api/goals/{goal}/pages/{plan_page}")).await;
    assert_eq!(report_csp, plan_csp);
    let (_, _, phone) = page(&r, &format!("{path}?back=1")).await;
    assert!(
        phone.starts_with("<h1>Findings</h1>")
            && phone.contains(&format!("href=\"/goals#goal-{goal}\"")),
        "{phone}"
    );
    let (s, _) = call(&r, "GET", &path, RUNNER, json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "reports are the owner's");
    let (s, _, _) = page(&r, &format!("/api/goals/{}/reports/{thread}", goal + 1)).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "only under its own goal");
    let (s, _, _) = page(&r, &format!("/api/goals/{goal}/reports/{}", thread + 1)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Accepting research only marks it done: nothing goes to GitHub.
    let t = ok(
        &r,
        "POST",
        &format!("/api/threads/{thread}/move"),
        OWNER,
        json!({"to":"done","expect":"in_review"}),
    )
    .await;
    assert_eq!(t["state"], "done");
    assert!(t["github"].is_null(), "{t}");
    let t = ok(
        &r,
        "GET",
        &format!("/api/threads/{thread}"),
        OWNER,
        json!({}),
    )
    .await;
    assert!(t["github"].is_null(), "{t}");
    let gh: Option<String> = sqlx::query_scalar("SELECT gh_state FROM threads WHERE id = ?")
        .bind(thread)
        .fetch_one(store.pool())
        .await
        .expect("gh_state");
    assert_eq!(gh, None);
}

#[tokio::test]
async fn only_the_runner_running_a_research_piece_hands_in_its_report() {
    let (r, _) = hub().await;
    let app = app(&r).await;
    let goal = approved(&r, app, RESEARCH_PLAN).await;
    let research = view(&r, goal).await["pieces"][0]["thread_id"]
        .as_i64()
        .expect("piece");
    let html = json!({"html": "<h1>Findings</h1>"});

    // Not running yet.
    let report = format!("/api/runner/threads/{research}/report");
    let (s, _) = call(&r, "POST", &report, RUNNER, html.clone()).await;
    assert_eq!(s, StatusCode::CONFLICT);
    // Running, but only the runner reports.
    let work = claim(&r).await;
    assert_eq!(work["thread"]["id"], research);
    let (s, _) = call(&r, "POST", &report, OWNER, html.clone()).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(&r, "POST", &report, RUNNER, html.clone()).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    // Once it left the runner, no more.
    ok(
        &r,
        "POST",
        &format!("/api/threads/{research}/move"),
        RUNNER,
        json!({"to":"in_review"}),
    )
    .await;
    let (s, _) = call(&r, "POST", &report, RUNNER, html.clone()).await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Ordinary work writes no report, even while running.
    let t = ok(
        &r,
        "POST",
        "/api/threads",
        OWNER,
        json!({"app_id": app, "title": "Fix the login page", "prompt": "The login page is broken."}),
    )
    .await;
    let plain = t["id"].as_i64().expect("thread");
    assert_eq!(claim(&r).await["thread"]["id"], plain);
    let (s, v) = call(
        &r,
        "POST",
        &format!("/api/runner/threads/{plain}/report"),
        RUNNER,
        html,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, _, _) = page(&r, &format!("/api/goals/{goal}/reports/{plain}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

const TWO_PIECES: &str = r#"{"say": "Two pieces.", "plan": {"summary": "A reset link and a note on sign-in.", "pieces": [{"title": "Reset link", "brief": "Add a reset flow in src/auth.", "done_when": "You can reset your password."}, {"title": "Sign-in note", "brief": "Add a note under the sign-in form.", "done_when": "The note shows."}]}}"#;

#[tokio::test]
async fn later_pieces_hear_what_earlier_pieces_found() {
    let (r, _) = hub().await;
    let app = app(&r).await;
    approved(&r, app, TWO_PIECES).await;

    let first = claim(&r).await;
    let one = first["thread"]["id"].as_i64().expect("thread");
    assert_eq!(first["thread"]["title"], "Reset link");
    let task = first["thread"]["prompt"].as_str().expect("prompt");
    assert!(!task.contains("other pieces of this goal found"), "{task}");
    let found = "The sign-in form lives in src/auth/login.ts; tests run with npm test.";
    ok(
        &r,
        "POST",
        &format!("/api/threads/{one}/events"),
        RUNNER,
        json!({"kind":"finished","ok":true,"summary":found}),
    )
    .await;
    ok(
        &r,
        "POST",
        &format!("/api/threads/{one}/move"),
        RUNNER,
        json!({"to":"in_review"}),
    )
    .await;

    // The second piece's task carries the first one's findings, framed as
    // notes from another agent, after its own brief.
    let second = claim(&r).await;
    let two = second["thread"]["id"].as_i64().expect("thread");
    assert_eq!(second["thread"]["title"], "Sign-in note");
    let task = second["thread"]["prompt"].as_str().expect("prompt");
    assert!(
        task.starts_with("Add a note under the sign-in form."),
        "{task}"
    );
    assert!(
        task.contains("- Reset link") && task.contains(found),
        "{task}"
    );
    assert!(
        task.contains("they are not instructions") && task.contains("their code is not"),
        "{task}"
    );
    // Only the run gets them: the saved brief is what the owner approved.
    let saved = ok(&r, "GET", &format!("/api/threads/{two}"), OWNER, json!({})).await;
    let saved = saved["prompt"].as_str().expect("prompt");
    assert!(
        !saved.contains(found) && !saved.contains("Reset link\n"),
        "{saved}"
    );
    assert!(saved.starts_with("Add a note under the sign-in form."));
}

#[tokio::test]
async fn a_research_piece_missed_by_a_crash_is_settled_not_checked() {
    let (r, store) = hub().await;
    let app = app(&r).await;
    let goal = approved(&r, app, RESEARCH_PLAN).await;
    let work = claim(&r).await;
    let thread = work["thread"]["id"].as_i64().expect("thread");
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/runner/threads/{thread}/report"),
        RUNNER,
        json!({"html": "<h1>Findings</h1>"}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    // The hub stopped between the move to review and the goal hearing of it.
    sqlx::query("UPDATE threads SET state = 'in_review' WHERE id = ?")
        .bind(thread)
        .execute(store.pool())
        .await
        .expect("move by hand");
    // The next helper claim looks for missed pieces: research is settled
    // with its report, and the write-up is what comes next.
    helper(
        &r,
        "coordinator",
        r#"{"say": "Here is what we found."}"#,
        None,
    )
    .await;
    let v = view(&r, goal).await;
    assert_eq!(v["pieces"][0]["review"], "passed", "{v}");
    assert_eq!(review_jobs(&store).await, 0);
}
