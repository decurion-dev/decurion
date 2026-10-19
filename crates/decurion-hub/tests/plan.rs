//! The master plan: each app's goals with their steps, what they found, and
//! the owner's own to-do items, with the runner's side played by the test.

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

async fn app(r: &axum::Router) -> i64 {
    let v = ok(
        r,
        "POST",
        "/api/apps",
        OWNER,
        json!({"name":"clinic","repo_path":"C:\\repos\\clinic","profile":"A"}),
    )
    .await;
    v["id"].as_i64().expect("app id")
}

async fn goal(r: &axum::Router, app: i64, text: &str) -> i64 {
    let (s, g) = call(
        r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": text}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{g}");
    g["id"].as_i64().expect("goal id")
}

/// Claims the next helper job, checks its kind, and finishes it with `text`.
async fn helper(r: &axum::Router, kind: &str, text: &str, page: Option<&str>) -> Value {
    let claim = ok(
        r,
        "POST",
        "/api/runner/helper/claim",
        RUNNER,
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(claim["kind"], kind, "{claim}");
    let id = claim["job_id"].as_i64().expect("job id");
    ok(
        r,
        "POST",
        &format!("/api/runner/helper/{id}/started"),
        RUNNER,
        json!({}),
    )
    .await;
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

async fn plan(r: &axum::Router, app: i64) -> Value {
    ok(r, "GET", &format!("/api/apps/{app}/plan"), OWNER, json!({})).await
}

/// The plan row of one goal.
fn goal_row(plan: &Value, goal: i64) -> Value {
    plan["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["kind"] == "goal" && i["goal"]["id"] == goal)
        .cloned()
        .unwrap_or_else(|| panic!("goal {goal} is not on the plan: {plan}"))
}

fn step_states(row: &Value) -> Vec<String> {
    row["steps"]
        .as_array()
        .map(|s| {
            s.iter()
                .map(|s| s["state"].as_str().expect("state").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn titles(items: &Value) -> Vec<String> {
    items
        .as_array()
        .map(|s| {
            s.iter()
                .map(|s| s["title"].as_str().expect("title").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

const PLAN: &str = r#"{"say": "Here is my plan.", "plan": {"summary": "Go through every visit screen and write down what is wrong.", "pieces": [{"title": "Audit visits", "brief": "Walk the visit workflow and list problems.", "done_when": "You have a list of what to fix."}]}}"#;

const FINDINGS: &str = r#"Done.
{"say": "The audit found five problems.", "todo": [
 {"title": "Check-in skips the insurance card", "detail": "The form never asks for it."},
 {"title": "Visit notes save twice"},
 {"title": "The end-visit button is hidden on phones"},
 {"title": "Times show in the wrong time zone"},
 {"title": "No warning before leaving an unsaved note"}]}"#;

#[tokio::test]
async fn a_goal_shows_in_its_plan_with_its_steps_as_it_moves() {
    let r = hub().await;
    let app = app(&r).await;
    let goal = goal(&r, app, "Audit the patient visit workflow").await;

    let p = plan(&r, app).await;
    assert_eq!(p["app_name"], "clinic");
    let row = goal_row(&p, goal);
    assert_eq!(row["title"], "Audit the patient visit workflow");
    assert_eq!(row["state"], "open");
    assert_eq!(row["source"], "owner");
    assert_eq!(
        titles(&row["steps"]),
        ["Talk it through", "Approve the plan", "Your review"]
    );
    assert_eq!(step_states(&row), ["now", "later", "later"]);
    assert_eq!((p["done"].as_i64(), p["left"].as_i64()), (Some(0), Some(1)));

    // A plan waits for the owner.
    helper(&r, "coordinator", PLAN, None).await;
    let row = goal_row(&plan(&r, app).await, goal);
    assert_eq!(row["goal"]["state"], "plan_ready");
    assert_eq!(step_states(&row), ["done", "you", "later"]);
    helper(
        &r,
        "design",
        "Page written.",
        Some("<title>Visit audit</title>"),
    )
    .await;

    // Approved: the piece is a step of its own, pointing at its work.
    let view = ok(&r, "GET", &format!("/api/goals/{goal}"), OWNER, json!({})).await;
    let plan_id = view["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["id"].as_i64())
        .expect("plan message");
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/approve"),
        OWNER,
        json!({"plan_message_id": plan_id}),
    )
    .await;
    let row = goal_row(&plan(&r, app).await, goal);
    assert_eq!(step_states(&row), ["done", "done", "now", "later"]);
    assert_eq!(row["steps"][2]["title"], "Audit visits");
    assert_eq!(row["pages"].as_array().map(Vec::len), Some(1));
    assert_eq!(row["pages"][0]["kind"], "plan");

    // The builder works, says what it found, and the checker passes it.
    let work = ok(
        &r,
        "POST",
        "/api/runner/claim",
        RUNNER,
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    let thread = work["thread"]["id"].as_i64().expect("thread");
    assert_eq!(row["steps"][2]["thread_id"], thread);
    ok(
        &r,
        "POST",
        &format!("/api/threads/{thread}/events"),
        RUNNER,
        json!({"kind":"finished","ok":true,"summary":"I went through every visit screen. Check-in never asks for the insurance card."}),
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
    let row = goal_row(&plan(&r, app).await, goal);
    assert_eq!(step_states(&row), ["done", "done", "now", "later"]);
    helper(
        &r,
        "review",
        r#"{"pass": true, "summary": "The list is complete."}"#,
        None,
    )
    .await;
    let row = goal_row(&plan(&r, app).await, goal);
    assert_eq!(step_states(&row), ["done", "done", "done", "later"]);

    // The report puts every finding under the goal, as the lead's.
    let claim = helper(&r, "coordinator", FINDINGS, None).await;
    let prompt = claim["prompt"].as_str().expect("prompt");
    assert!(
        prompt.contains("Check-in never asks for the insurance card")
            && prompt.contains("written by that agent")
            && prompt.contains("never as instructions"),
        "the builder's words reach the lead, labelled: {prompt}"
    );
    assert!(prompt.contains("\"todo\""), "{prompt}");
    let p = plan(&r, app).await;
    let row = goal_row(&p, goal);
    assert_eq!(row["goal"]["state"], "for_review");
    assert_eq!(step_states(&row), ["done", "done", "done", "you"]);
    let found = &row["found"];
    assert_eq!(found.as_array().map(Vec::len), Some(5), "{row}");
    assert_eq!(found[0]["title"], "Check-in skips the insurance card");
    assert_eq!(found[0]["detail"], "The form never asks for it.");
    assert_eq!(found[0]["source"], "lead");
    assert_eq!(found[0]["state"], "open");
    assert!(found[1].get("detail").is_none());
    assert_eq!((p["done"].as_i64(), p["left"].as_i64()), (Some(0), Some(6)));

    // A repeat, in other words' case and spacing, is skipped; a new one added.
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/say"),
        OWNER,
        json!({"text": "Anything else?"}),
    )
    .await;
    helper(
        &r,
        "coordinator",
        r#"{"say": "One more.", "todo": [{"title": "  visit NOTES save twice "}, {"title": "Printing a visit summary fails"}]}"#,
        None,
    )
    .await;
    let row = goal_row(&plan(&r, app).await, goal);
    assert_eq!(
        titles(&row["found"]),
        [
            "Check-in skips the insurance card",
            "Visit notes save twice",
            "The end-visit button is hidden on phones",
            "Times show in the wrong time zone",
            "No warning before leaving an unsaved note",
            "Printing a visit summary fails",
        ]
    );

    // Closing the goal ticks its row; its findings stay to do.
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/close"),
        OWNER,
        json!({}),
    )
    .await;
    let p = plan(&r, app).await;
    let row = goal_row(&p, goal);
    assert_eq!(row["state"], "done");
    assert!(row["done_at"].is_string());
    assert_eq!(step_states(&row), ["done", "done", "done", "done"]);
    assert_eq!((p["done"].as_i64(), p["left"].as_i64()), (Some(1), Some(6)));
}

#[tokio::test]
async fn a_stopped_goal_is_dropped_and_skips_its_steps() {
    let r = hub().await;
    let app = app(&r).await;
    let goal = goal(&r, app, "Something we stop").await;
    ok(
        &r,
        "POST",
        &format!("/api/goals/{goal}/stop"),
        OWNER,
        json!({}),
    )
    .await;
    let p = plan(&r, app).await;
    let row = goal_row(&p, goal);
    assert_eq!(row["state"], "dropped");
    assert_eq!(step_states(&row), ["skipped", "skipped", "skipped"]);
    assert_eq!((p["done"].as_i64(), p["left"].as_i64()), (Some(0), Some(0)));
}

#[tokio::test]
async fn the_owner_adds_ticks_rewords_and_reorders_to_dos() {
    let r = hub().await;
    let app = app(&r).await;
    let goal = goal(&r, app, "Audit visits").await;
    let goal_item = goal_row(&plan(&r, app).await, goal)["id"]
        .as_i64()
        .expect("row id");

    // At the end of the plan, once per receipt.
    let (s, a) = call(
        &r,
        "POST",
        &format!("/api/apps/{app}/plan/items"),
        OWNER,
        json!({"title": "  Tidy the menu ", "detail": "It has too many items.", "receipt": "todo-0001"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    assert_eq!(a["title"], "Tidy the menu");
    assert_eq!(a["kind"], "todo");
    assert_eq!(a["source"], "owner");
    assert_eq!(a["state"], "open");
    let (s, again) = call(
        &r,
        "POST",
        &format!("/api/apps/{app}/plan/items"),
        OWNER,
        json!({"title": "Tidy the menu", "receipt": "todo-0001"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "a resend answers with the first result");
    assert_eq!(again["id"], a["id"]);
    let b = ok(
        &r,
        "POST",
        &format!("/api/apps/{app}/plan/items"),
        OWNER,
        json!({"title": "Add a dark mode"}),
    )
    .await;
    // Under the goal.
    let c = ok(
        &r,
        "POST",
        &format!("/api/apps/{app}/plan/items"),
        OWNER,
        json!({"title": "Ask the front desk", "parent_id": goal_item}),
    )
    .await;
    let p = plan(&r, app).await;
    assert_eq!(
        titles(&p["items"]),
        ["Audit visits", "Tidy the menu", "Add a dark mode"]
    );
    assert_eq!(titles(&goal_row(&p, goal)["found"]), ["Ask the front desk"]);
    assert_eq!(p["left"], 4);

    // Bad words and bad places are refused in plain words.
    let (s, v) = call(
        &r,
        "POST",
        &format!("/api/apps/{app}/plan/items"),
        OWNER,
        json!({"title": "   "}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v, "Write what needs doing.");
    let (s, v) = call(
        &r,
        "POST",
        &format!("/api/apps/{app}/plan/items"),
        OWNER,
        json!({"title": "x".repeat(201)}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(v.as_str().expect("words").contains("200 characters"), "{v}");
    let (s, v) = call(
        &r,
        "POST",
        &format!("/api/apps/{app}/plan/items"),
        OWNER,
        json!({"title": "Under a to-do", "parent_id": b["id"]}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(v.as_str().expect("words").contains("goals"), "{v}");
    let other = ok(
        &r,
        "POST",
        "/api/apps",
        OWNER,
        json!({"name":"shop","repo_path":"C:\\repos\\shop","profile":"A"}),
    )
    .await;
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/apps/{}/plan/items", other["id"]),
        OWNER,
        json!({"title": "Wrong app", "parent_id": goal_item}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "a goal of another app");

    // Tick, reopen, reword.
    let id = a["id"].as_i64().expect("id");
    let t = ok(
        &r,
        "POST",
        &format!("/api/plan/items/{id}"),
        OWNER,
        json!({"state": "done"}),
    )
    .await;
    assert_eq!(t["state"], "done");
    let ticked_at = t["done_at"].as_str().expect("done_at").to_owned();
    let t = ok(
        &r,
        "POST",
        &format!("/api/plan/items/{id}"),
        OWNER,
        json!({"title": "Tidy the main menu"}),
    )
    .await;
    assert_eq!(t["title"], "Tidy the main menu");
    assert_eq!(t["detail"], "It has too many items.");
    assert_eq!(t["done_at"], ticked_at.as_str(), "rewording keeps the tick");
    let p = plan(&r, app).await;
    assert_eq!((p["done"].as_i64(), p["left"].as_i64()), (Some(1), Some(3)));
    let t = ok(
        &r,
        "POST",
        &format!("/api/plan/items/{id}"),
        OWNER,
        json!({"state": "open", "detail": ""}),
    )
    .await;
    assert_eq!(t["state"], "open");
    assert!(t.get("done_at").is_none(), "{t}");
    assert!(t.get("detail").is_none(), "{t}");
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/plan/items/{id}"),
        OWNER,
        json!({"title": ""}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = call(
        &r,
        "POST",
        &format!("/api/plan/items/{goal_item}"),
        OWNER,
        json!({"state": "done"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(
        v.as_str().expect("words").contains("when you close it"),
        "{v}"
    );
    let (s, _) = call(
        &r,
        "POST",
        "/api/plan/items/9999",
        OWNER,
        json!({"state": "done"}),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Dropped rows count neither way.
    ok(
        &r,
        "POST",
        &format!("/api/plan/items/{}", c["id"]),
        OWNER,
        json!({"state": "dropped"}),
    )
    .await;
    let p = plan(&r, app).await;
    assert_eq!((p["done"].as_i64(), p["left"].as_i64()), (Some(0), Some(3)));

    // Reorder among the rows beside it; nothing moves past either end.
    let p = ok(
        &r,
        "POST",
        &format!("/api/plan/items/{}/move", b["id"]),
        OWNER,
        json!({"direction": "up"}),
    )
    .await;
    assert_eq!(
        titles(&p["items"]),
        ["Audit visits", "Add a dark mode", "Tidy the main menu"]
    );
    let p = ok(
        &r,
        "POST",
        &format!("/api/plan/items/{}/move", b["id"]),
        OWNER,
        json!({"direction": "up"}),
    )
    .await;
    assert_eq!(
        titles(&p["items"]),
        ["Add a dark mode", "Audit visits", "Tidy the main menu"]
    );
    let p = ok(
        &r,
        "POST",
        &format!("/api/plan/items/{}/move", b["id"]),
        OWNER,
        json!({"direction": "up"}),
    )
    .await;
    assert_eq!(
        titles(&p["items"]),
        ["Add a dark mode", "Audit visits", "Tidy the main menu"]
    );
    let p = ok(
        &r,
        "POST",
        &format!("/api/plan/items/{goal_item}/move"),
        OWNER,
        json!({"direction": "down"}),
    )
    .await;
    assert_eq!(
        titles(&p["items"]),
        ["Add a dark mode", "Tidy the main menu", "Audit visits"]
    );
    // The only row under its goal stays where it is.
    let p = ok(
        &r,
        "POST",
        &format!("/api/plan/items/{}/move", c["id"]),
        OWNER,
        json!({"direction": "down"}),
    )
    .await;
    assert_eq!(titles(&goal_row(&p, goal)["found"]), ["Ask the front desk"]);
    // A new goal still goes at the end.
    let later = goal_row_after(&r, app, "Later goal").await;
    assert_eq!(later, "Later goal");
}

async fn goal_row_after(r: &axum::Router, app: i64, text: &str) -> String {
    goal(r, app, text).await;
    let p = plan(r, app).await;
    p["items"]
        .as_array()
        .and_then(|i| i.last())
        .and_then(|i| i["title"].as_str())
        .expect("last row")
        .to_owned()
}

/// A goal whose lead found five things while talking, with the found items'
/// ids.
async fn goal_with_findings(r: &axum::Router, app: i64) -> (i64, Vec<i64>) {
    let goal = goal(r, app, "Audit the patient visit workflow").await;
    helper(r, "coordinator", FINDINGS, None).await;
    let row = goal_row(&plan(r, app).await, goal);
    let ids = row["found"]
        .as_array()
        .expect("found")
        .iter()
        .map(|i| i["id"].as_i64().expect("id"))
        .collect();
    (goal, ids)
}

#[tokio::test]
async fn fixing_to_dos_starts_a_linked_goal_and_closing_it_ticks_them() {
    let r = hub().await;
    let app = app(&r).await;
    let (audit, found) = goal_with_findings(&r, app).await;

    let (s, g) = call(
        &r,
        "POST",
        &format!("/api/apps/{app}/plan/fix"),
        OWNER,
        json!({"item_ids": [found[0], found[1]], "note": "Start with check-in.", "receipt": "fix-00001"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{g}");
    let fix = g["id"].as_i64().expect("goal id");
    assert_eq!(g["state"], "talking");
    assert_eq!(g["busy"], true, "the lead starts on it");
    assert_eq!(g["title"], "Check-in skips the insurance card (and 1 more)");
    let view = ok(&r, "GET", &format!("/api/goals/{fix}"), OWNER, json!({})).await;
    let first = view["messages"][0]["body"]["text"].as_str().expect("text");
    assert_eq!(
        first,
        "Please fix these, found while working on \"Audit the patient visit workflow\":\n\
         - Check-in skips the insurance card: The form never asks for it.\n\
         - Visit notes save twice\n\nStart with check-in."
    );

    // A resent tap answers with the same goal and starts nothing new.
    let (s, again) = call(
        &r,
        "POST",
        &format!("/api/apps/{app}/plan/fix"),
        OWNER,
        json!({"item_ids": [found[0], found[1]], "receipt": "fix-00001"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(again["id"], fix);
    let goals = ok(&r, "GET", "/api/goals", OWNER, json!({})).await;
    assert_eq!(goals.as_array().map(Vec::len), Some(2));

    // The items point at their fix goal, which has its own row at the end.
    let p = plan(&r, app).await;
    let row = goal_row(&p, audit);
    assert_eq!(row["found"][0]["goal"]["id"], fix);
    assert_eq!(row["found"][1]["goal"]["id"], fix);
    assert!(row["found"][2].get("goal").is_none());
    // The job counts once: the audit and 3 findings left, plus the fix goal.
    assert_eq!((p["done"].as_i64(), p["left"].as_i64()), (Some(0), Some(5)));
    assert_eq!(
        titles(&p["items"]),
        [
            "Audit the patient visit workflow",
            "Check-in skips the insurance card (and 1 more)"
        ]
    );

    // An item already being fixed is refused, alone or with others.
    let (s, v) = call(
        &r,
        "POST",
        &format!("/api/apps/{app}/plan/fix"),
        OWNER,
        json!({"item_ids": [found[2], found[1]]}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(
        v.as_str()
            .expect("words")
            .contains("already being fixed in another goal"),
        "{v}"
    );
    let audit_item = row["id"].as_i64().expect("row");
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/apps/{app}/plan/fix"),
        OWNER,
        json!({"item_ids": [audit_item]}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "a goal row is not a to-do");
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/apps/{app}/plan/fix"),
        OWNER,
        json!({"item_ids": []}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let many: Vec<i64> = (0..11).map(|i| 1000 + i).collect();
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/apps/{app}/plan/fix"),
        OWNER,
        json!({"item_ids": many}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let shop = ok(
        &r,
        "POST",
        "/api/apps",
        OWNER,
        json!({"name":"shop","repo_path":"C:\\repos\\shop","profile":"A"}),
    )
    .await;
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/apps/{}/plan/fix", shop["id"]),
        OWNER,
        json!({"item_ids": [found[3]]}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "another app's to-do");

    // Closing the fix goal ticks what it fixed, and only that.
    ok(
        &r,
        "POST",
        &format!("/api/goals/{fix}/close"),
        OWNER,
        json!({}),
    )
    .await;
    let p = plan(&r, app).await;
    let row = goal_row(&p, audit);
    assert_eq!(row["found"][0]["state"], "done");
    assert!(row["found"][0]["done_at"].is_string());
    assert_eq!(row["found"][1]["state"], "done");
    assert_eq!(row["found"][2]["state"], "open");
    // Audit open + 3 findings left; only the fix goal is done, the two ticked
    // findings are carried by it and not counted again.
    assert_eq!((p["done"].as_i64(), p["left"].as_i64()), (Some(1), Some(4)));

    // A ticked item cannot be fixed again until it is reopened.
    let (s, _) = call(
        &r,
        "POST",
        &format!("/api/apps/{app}/plan/fix"),
        OWNER,
        json!({"item_ids": [found[0]]}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Stopping a fix goal leaves its items open, ready to be fixed again.
    let g = ok(
        &r,
        "POST",
        &format!("/api/apps/{app}/plan/fix"),
        OWNER,
        json!({"item_ids": [found[2]]}),
    )
    .await;
    assert_eq!(g["title"], "The end-visit button is hidden on phones");
    ok(
        &r,
        "POST",
        &format!("/api/goals/{}/stop", g["id"]),
        OWNER,
        json!({}),
    )
    .await;
    let p = plan(&r, app).await;
    let row = goal_row(&p, audit);
    assert_eq!(row["found"][2]["state"], "open");
    // Stopped: the to-do is open and counted again; the stopped goal is not.
    assert_eq!((p["done"].as_i64(), p["left"].as_i64()), (Some(1), Some(4)));
    let again = ok(
        &r,
        "POST",
        &format!("/api/apps/{app}/plan/fix"),
        OWNER,
        json!({"item_ids": [found[2]]}),
    )
    .await;
    assert_ne!(again["id"], g["id"]);
    let p = plan(&r, app).await;
    let row = goal_row(&p, audit);
    assert_eq!(row["found"][2]["goal"]["id"], again["id"]);
    // Handed off again: the new goal row stands in for the to-do.
    assert_eq!((p["done"].as_i64(), p["left"].as_i64()), (Some(1), Some(4)));
}

#[tokio::test]
async fn only_the_owner_reads_or_changes_the_plan() {
    let r = hub().await;
    let app = app(&r).await;
    let (_, found) = goal_with_findings(&r, app).await;
    let item = found[0];
    for (method, path, body) in [
        ("GET", format!("/api/apps/{app}/plan"), json!({})),
        (
            "POST",
            format!("/api/apps/{app}/plan/items"),
            json!({"title": "Sneaky"}),
        ),
        (
            "POST",
            format!("/api/plan/items/{item}"),
            json!({"state": "done"}),
        ),
        (
            "POST",
            format!("/api/plan/items/{item}/move"),
            json!({"direction": "up"}),
        ),
        (
            "POST",
            format!("/api/apps/{app}/plan/fix"),
            json!({"item_ids": [item]}),
        ),
    ] {
        let (s, v) = call(&r, method, &path, RUNNER, body).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{method} {path}: {v}");
    }
    // Nothing changed.
    let p = plan(&r, app).await;
    assert_eq!(p["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(p["items"][0]["found"][0]["state"], "open");
    let goals = ok(&r, "GET", "/api/goals", OWNER, json!({})).await;
    assert_eq!(goals.as_array().map(Vec::len), Some(1));
}
