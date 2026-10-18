//! Where new work starts: the app's default branch as its git server has it,
//! not whatever branch the owner's folder happens to be on. Real git repos
//! stand in for the owner's folder and its server.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::checkpoints::Files;
use decurion_hub::{router, store::Store, AppState, Keys};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";

struct Hub {
    r: axum::Router,
    store: Store,
    files: std::sync::Arc<Files>,
    dir: PathBuf,
}

impl Hub {
    /// Long enough later that the hub asks the git server again rather
    /// than use its last answer.
    fn later(&self) {
        self.files.age_answers();
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn hub() -> Hub {
    let dir = std::env::temp_dir().join(format!("decurion-start-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let state = AppState::new(
        store.clone(),
        Keys {
            owner: OWNER.into(),
            runner: RUNNER.into(),
        },
        "abc123".into(),
        Some("https://pc.tail.ts.net:8443".into()),
    )
    .with_files(dir.join("saved"));
    let files = state.files().expect("files");
    let r = router(state);
    Hub {
        r,
        store,
        files,
        dir,
    }
}

struct Resp {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    bytes: Vec<u8>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&self.bytes).into()))
    }
}

async fn send(r: &axum::Router, method: &str, path: &str, key: &str, body: Value) -> Resp {
    let mut req = Request::builder().method(method).uri(path);
    if !key.is_empty() {
        req = req.header(header::AUTHORIZATION, format!("Bearer {key}"));
    }
    let req = req
        .header(header::CONTENT_TYPE, "application/json")
        .body(if method == "GET" {
            Body::empty()
        } else {
            Body::from(body.to_string())
        })
        .expect("request");
    let resp = r.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes()
        .to_vec();
    Resp {
        status,
        headers,
        bytes,
    }
}

async fn ok(r: &axum::Router, method: &str, path: &str, key: &str, body: Value) -> Value {
    let resp = send(r, method, path, key, body).await;
    assert!(
        resp.status.is_success(),
        "{method} {path}: {} {}",
        resp.status,
        resp.json()
    );
    resp.json()
}

/// git for the test's own repos, with a fixed author. Returns its output.
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
            "-c",
            "init.defaultBranch=trunk",
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

/// Makes a commit in `repo` and returns its id.
fn commit(repo: &Path, file: &str, text: &str) -> String {
    std::fs::write(repo.join(file), text).expect("write");
    git(repo, &["add", "."]);
    git(repo, &["commit", "--quiet", "-m", text]);
    git(repo, &["rev-parse", "HEAD"])
}

/// The owner's setup: a server ("origin") whose default branch is `trunk`
/// at `b`, and the owner's folder cloned from it with every local hint
/// pointing elsewhere: local trunk reset to the older `a`, the folder
/// switched to a side branch at `c`, and its saved idea of the server's
/// default (`origin/HEAD`) pointing at a stale branch `old`.
struct Setup {
    seed: PathBuf,
    origin: PathBuf,
    folder: PathBuf,
    a: String,
    b: String,
    c: String,
}

fn setup(dir: &Path) -> Setup {
    let seed = dir.join("seed");
    std::fs::create_dir_all(&seed).expect("seed dir");
    git(&seed, &["init", "--quiet"]);
    let a = commit(&seed, "README.md", "a");
    let b = commit(&seed, "README.md", "b");
    let origin = dir.join("origin.git");
    git(
        dir,
        &[
            "clone",
            "--quiet",
            "--bare",
            &seed.to_string_lossy(),
            &origin.to_string_lossy(),
        ],
    );
    git(&origin, &["branch", "old", &a]);
    let folder = dir.join("app");
    git(
        dir,
        &[
            "clone",
            "--quiet",
            &origin.to_string_lossy(),
            &folder.to_string_lossy(),
        ],
    );
    git(&folder, &["reset", "--quiet", "--hard", &a]);
    git(&folder, &["switch", "--quiet", "-c", "side"]);
    let c = commit(&folder, "side.txt", "c");
    git(
        &folder,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/old",
        ],
    );
    Setup {
        seed,
        origin,
        folder,
        a,
        b,
        c,
    }
}

impl Setup {
    /// A new commit on the server's default branch.
    fn advance(&self, text: &str) -> String {
        let sha = commit(&self.seed, "README.md", text);
        git(
            &self.origin,
            &[
                "fetch",
                "--quiet",
                &self.seed.to_string_lossy(),
                "+trunk:trunk",
            ],
        );
        sha
    }

    /// The server goes away.
    fn unreachable(&self) {
        std::fs::rename(&self.origin, self.origin.with_extension("gone")).expect("move origin");
    }
}

async fn add_app(h: &Hub, folder: &Path) -> i64 {
    let app = ok(
        &h.r,
        "POST",
        "/api/apps",
        OWNER,
        json!({"name": "shop", "repo_path": folder.to_string_lossy(), "profile": "A"}),
    )
    .await;
    app["id"].as_i64().expect("app id")
}

async fn new_work(h: &Hub, app: i64) -> i64 {
    let t = ok(
        &h.r,
        "POST",
        "/api/threads",
        OWNER,
        json!({"app_id": app, "title": "t", "prompt": "do it", "place": "pc"}),
    )
    .await;
    t["id"].as_i64().expect("thread id")
}

/// The commit a piece of work starts from, as the PC runner is told it.
async fn start_of(h: &Hub, thread: i64) -> String {
    let resp = send(
        &h.r,
        "GET",
        &format!("/api/runner/threads/{thread}/base"),
        RUNNER,
        Value::Null,
    )
    .await;
    assert_eq!(resp.status, StatusCode::OK, "{}", resp.json());
    resp.headers["x-decurion-base"]
        .to_str()
        .expect("header")
        .to_owned()
}

/// The one commit a starting bundle holds.
fn bundle_head(path: &Path) -> String {
    let heads = git(
        Path::new("."),
        &["bundle", "list-heads", &path.to_string_lossy()],
    );
    let (sha, name) = heads.split_once(' ').expect("one head");
    assert_eq!(name, "refs/heads/decurion-base");
    sha.to_owned()
}

#[tokio::test]
async fn new_work_starts_from_the_default_branch_on_the_apps_git_server() {
    let h = hub().await;
    let s = setup(&h.dir);
    let app = add_app(&h, &s.folder).await;

    // Not the folder's local trunk (a), its side branch (c), or the stale
    // default it remembers (old, at a).
    let first = start_of(&h, new_work(&h, app).await).await;
    assert_eq!(first, s.b, "a = {}, c = {}", s.a, s.c);

    // The server's answer lands in a copy of its own, which borrows what the
    // hub's copy of the app already holds; the hub's copy only takes the
    // one branch from it, locally.
    let repos = h.dir.join("saved").join("repos");
    let alternates = std::fs::read_to_string(
        repos
            .join(format!("app-{app}-upstream.git"))
            .join("objects")
            .join("info")
            .join("alternates"),
    )
    .expect("the server's copy borrows the hub's objects");
    assert!(
        alternates
            .trim()
            .replace('\\', "/")
            .ends_with(&format!("app-{app}.git/objects")),
        "{alternates}"
    );
    assert!(git(
        &repos.join(format!("app-{app}.git")),
        &["for-each-ref", "--format=%(objectname)", "refs/upstream/"]
    )
    .contains(&s.b));

    // The server moves on: so does the next new piece, once the hub's
    // last answer is old.
    let b2 = s.advance("b2");
    assert_eq!(start_of(&h, new_work(&h, app).await).await, s.b);
    h.later();
    assert_eq!(start_of(&h, new_work(&h, app).await).await, b2);

    // The server cannot be reached: the last default it gave.
    s.unreachable();
    assert_eq!(start_of(&h, new_work(&h, app).await).await, b2);

    // No server at all: the folder's own main, then whatever it has out.
    git(&s.folder, &["remote", "remove", "origin"]);
    git(&s.folder, &["branch", "main", &s.a]);
    assert_eq!(start_of(&h, new_work(&h, app).await).await, s.a);
    git(&s.folder, &["branch", "-D", "main"]);
    assert_eq!(start_of(&h, new_work(&h, app).await).await, s.c);

    // Work keeps the start it was given, whatever happens after.
    let t = new_work(&h, app).await;
    let fixed = start_of(&h, t).await;
    commit(&s.folder, "side.txt", "c2");
    assert_eq!(start_of(&h, t).await, fixed);
}

#[tokio::test]
async fn a_new_folder_never_starts_from_the_old_folders_saved_default() {
    let h = hub().await;
    let s = setup(&h.dir);
    let app = add_app(&h, &s.folder).await;
    assert_eq!(start_of(&h, new_work(&h, app).await).await, s.b);

    // Another repo, with an origin of the same address, becomes the app's
    // folder, and that address stops answering.
    let other = h.dir.join("other");
    std::fs::create_dir_all(&other).expect("other dir");
    git(&other, &["init", "--quiet", "-b", "main"]);
    let m = commit(&other, "x.txt", "m");
    git(
        &other,
        &["remote", "add", "origin", &s.origin.to_string_lossy()],
    );
    ok(
        &h.r,
        "POST",
        &format!("/api/apps/{app}/edit"),
        OWNER,
        json!({"repo_path": other.to_string_lossy()}),
    )
    .await;
    s.unreachable();
    assert_eq!(start_of(&h, new_work(&h, app).await).await, m);
}

#[tokio::test]
async fn a_late_request_gets_the_bundle_of_the_start_that_won() {
    let h = hub().await;
    let s = setup(&h.dir);
    let app = add_app(&h, &s.folder).await;
    let t = new_work(&h, app).await;
    let folder = s.folder.to_string_lossy().into_owned();

    // One request bundles the default as it is now...
    let files = Files::new(h.dir.join("saved"));
    let late = files.base_bundle(app, &folder, None).await.expect("bundle");
    assert_eq!(late.0, s.b);
    // ...the default moves on, and another request fixes the start first.
    let b2 = s.advance("b2");
    assert_eq!(start_of(&h, t).await, b2);

    // The late one answers with the winner, header and contents alike.
    let (sha, path) = files
        .settle_base(&h.store, t, app, &folder, late)
        .await
        .expect("settle");
    assert_eq!(sha, b2);
    assert_eq!(bundle_head(&path), b2);
}

/// A goal through to one approved piece, with the runner's side played here.
async fn approve_one_piece(h: &Hub, goal: i64) -> i64 {
    let claim = ok(
        &h.r,
        "POST",
        "/api/runner/helper/claim",
        RUNNER,
        json!({"runner": "local", "rules_version": decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(claim["kind"], "coordinator", "{claim}");
    let id = claim["job_id"].as_i64().expect("job id");
    ok(
        &h.r,
        "POST",
        &format!("/api/runner/helper/{id}/started"),
        RUNNER,
        json!({}),
    )
    .await;
    let plan = r#"{"say": "Here is my plan.", "plan": {"summary": "One thing.", "pieces": [{"title": "Do it", "brief": "Change src.", "done_when": "It is done."}]}}"#;
    let resp = send(
        &h.r,
        "POST",
        &format!("/api/runner/helper/{id}/finish"),
        RUNNER,
        json!({"ok": true, "text": plan, "cost_usd": 0.01}),
    )
    .await;
    assert_eq!(resp.status, StatusCode::NO_CONTENT, "{}", resp.json());
    let v = ok(&h.r, "GET", &format!("/api/goals/{goal}"), OWNER, json!({})).await;
    let plan_id = v["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["id"].as_i64())
        .expect("plan message");
    ok(
        &h.r,
        "POST",
        &format!("/api/goals/{goal}/approve"),
        OWNER,
        json!({"plan_message_id": plan_id}),
    )
    .await;
    let v = ok(&h.r, "GET", &format!("/api/goals/{goal}"), OWNER, json!({})).await;
    v["pieces"][0]["thread_id"].as_i64().expect("piece thread")
}

/// The commit a goal's project lead and designer are given.
async fn goal_start(h: &Hub, goal: i64) -> String {
    let resp = send(
        &h.r,
        "GET",
        &format!("/api/runner/goals/{goal}/base"),
        RUNNER,
        Value::Null,
    )
    .await;
    assert_eq!(resp.status, StatusCode::OK, "{}", resp.json());
    let sha = resp.headers["x-decurion-base"]
        .to_str()
        .expect("header")
        .to_owned();
    let bundle = h.dir.join(format!("goal-{goal}.bundle"));
    std::fs::write(&bundle, &resp.bytes).expect("save bundle");
    assert_eq!(
        bundle_head(&bundle),
        sha,
        "the bundle holds the header's commit"
    );
    sha
}

#[tokio::test]
async fn a_goal_and_all_its_pieces_start_from_one_commit() {
    let h = hub().await;
    let s = setup(&h.dir);
    let app = add_app(&h, &s.folder).await;
    let g = ok(
        &h.r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "Add a page"}),
    )
    .await;
    let goal = g["id"].as_i64().expect("goal id");
    assert_eq!(
        h.store.goal_start_commit(goal).await.expect("read"),
        Some(s.b.clone())
    );

    // The server moves on while the goal is planned: the goal does not.
    s.advance("b2");
    assert_eq!(goal_start(&h, goal).await, s.b);
    let piece = approve_one_piece(&h, goal).await;
    assert_eq!(
        h.store.base_commit(piece).await.expect("read"),
        Some(s.b.clone())
    );
    assert_eq!(start_of(&h, piece).await, s.b);
    assert_eq!(goal_start(&h, goal).await, s.b);

    // The project lead's copy is made from that commit.
    let claim = ok(
        &h.r,
        "POST",
        "/api/runner/helper/claim",
        RUNNER,
        json!({"runner": "local", "rules_version": decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(claim["kind"], "design", "{claim}");
    assert_eq!(
        claim["source"],
        json!({"kind": "app_copy", "goal_id": goal, "start_commit": s.b})
    );
}

/// Claims the next helper job, checks its kind, and finishes it with `text`.
async fn finish_next(h: &Hub, kind: &str, text: &str) -> Value {
    let claim = ok(
        &h.r,
        "POST",
        "/api/runner/helper/claim",
        RUNNER,
        json!({"runner": "local", "rules_version": decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(claim["kind"], kind, "{claim}");
    let id = claim["job_id"].as_i64().expect("job id");
    ok(
        &h.r,
        "POST",
        &format!("/api/runner/helper/{id}/started"),
        RUNNER,
        json!({}),
    )
    .await;
    let resp = send(
        &h.r,
        "POST",
        &format!("/api/runner/helper/{id}/finish"),
        RUNNER,
        json!({"ok": true, "text": text, "cost_usd": 0.01, "page_html": "<h1>plan</h1>"}),
    )
    .await;
    assert_eq!(resp.status, StatusCode::NO_CONTENT, "{}", resp.json());
    claim
}

#[tokio::test]
async fn a_later_round_of_a_goal_starts_from_the_default_branch_as_it_is_then() {
    let h = hub().await;
    let s = setup(&h.dir);
    let app = add_app(&h, &s.folder).await;
    let g = ok(
        &h.r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "Add a page"}),
    )
    .await;
    let goal = g["id"].as_i64().expect("goal id");
    let first = approve_one_piece(&h, goal).await;
    finish_next(&h, "design", "drawn").await;
    // Every piece is finished and the owner looks at it.
    sqlx::query("UPDATE goals SET state = 'for_review' WHERE id = ?")
        .bind(goal)
        .execute(h.store.pool())
        .await
        .expect("finish the round");

    // The owner wants more; meanwhile the app's main branch moved on.
    let b2 = s.advance("b2");
    h.later();
    ok(
        &h.r,
        "POST",
        &format!("/api/goals/{goal}/say"),
        OWNER,
        json!({"text": "Now add a footer too"}),
    )
    .await;
    let plan = r#"{"say": "Next.", "plan": {"summary": "A footer.", "pieces": [{"title": "Footer", "brief": "Add it.", "done_when": "It shows."}]}}"#;
    finish_next(&h, "coordinator", plan).await;
    let v = ok(&h.r, "GET", &format!("/api/goals/{goal}"), OWNER, json!({})).await;
    let plan_id = v["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["id"].as_i64())
        .expect("plan message");
    let v = ok(
        &h.r,
        "POST",
        &format!("/api/goals/{goal}/approve"),
        OWNER,
        json!({"plan_message_id": plan_id}),
    )
    .await;
    assert_eq!(v["state"], "working", "{v}");
    let v = ok(&h.r, "GET", &format!("/api/goals/{goal}"), OWNER, json!({})).await;
    let second = v["pieces"]
        .as_array()
        .and_then(|p| p.iter().find(|p| p["thread_id"] != json!(first)))
        .and_then(|p| p["thread_id"].as_i64())
        .expect("the new piece");

    // The new piece and the goal's helpers move on; the earlier piece keeps
    // the start it was built on.
    assert_eq!(start_of(&h, second).await, b2);
    assert_eq!(start_of(&h, first).await, s.b);
    assert_eq!(
        h.store.goal_start_commit(goal).await.expect("read"),
        Some(b2.clone())
    );
    assert_eq!(goal_start(&h, goal).await, b2);
}

#[tokio::test]
async fn a_goal_started_from_to_dos_starts_from_the_default_branch_too() {
    let h = hub().await;
    let s = setup(&h.dir);
    let app = add_app(&h, &s.folder).await;
    let item = ok(
        &h.r,
        "POST",
        &format!("/api/apps/{app}/plan/items"),
        OWNER,
        json!({"title": "Tidy the menu"}),
    )
    .await;
    let g = ok(
        &h.r,
        "POST",
        &format!("/api/apps/{app}/plan/fix"),
        OWNER,
        json!({"item_ids": [item["id"]]}),
    )
    .await;
    let goal = g["id"].as_i64().expect("goal id");
    assert_eq!(
        h.store.goal_start_commit(goal).await.expect("read"),
        Some(s.b.clone())
    );
    let claim = ok(
        &h.r,
        "POST",
        "/api/runner/helper/claim",
        RUNNER,
        json!({"runner": "local", "rules_version": decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(
        claim["source"],
        json!({"kind": "app", "goal_id": goal, "start_commit": s.b})
    );
}

#[tokio::test]
async fn a_refused_or_resent_goal_request_never_waits_on_the_server() {
    let h = hub().await;
    let s = setup(&h.dir);
    let app = add_app(&h, &s.folder).await;
    let item = ok(
        &h.r,
        "POST",
        &format!("/api/apps/{app}/plan/items"),
        OWNER,
        json!({"title": "Tidy the menu"}),
    )
    .await;
    let fix = json!({"item_ids": [item["id"]], "receipt": "fix-00001"});
    let first = ok(
        &h.r,
        "POST",
        &format!("/api/apps/{app}/plan/fix"),
        OWNER,
        fix.clone(),
    )
    .await;

    // From now on the server takes half a minute to answer.
    git(
        &s.folder,
        &[
            "config",
            "remote.origin.uploadpack",
            "sleep 30; git-upload-pack",
        ],
    );
    let quick = |what: &str, started: std::time::Instant| {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "{what} waited on the server"
        );
    };
    let t = std::time::Instant::now();
    let again = send(
        &h.r,
        "POST",
        &format!("/api/apps/{app}/plan/fix"),
        OWNER,
        fix,
    )
    .await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.json()["id"], first["id"]);
    quick("a resent fix", t);

    for (body, want) in [
        (json!({"item_ids": []}), StatusCode::BAD_REQUEST),
        (json!({"item_ids": [item["id"]]}), StatusCode::CONFLICT),
        (
            json!({"item_ids": [item["id"]], "receipt": "x"}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let t = std::time::Instant::now();
        let resp = send(
            &h.r,
            "POST",
            &format!("/api/apps/{app}/plan/fix"),
            OWNER,
            body.clone(),
        )
        .await;
        assert_eq!(resp.status, want, "{body}: {}", resp.json());
        quick("a refused fix", t);
    }
    let t = std::time::Instant::now();
    let resp = send(
        &h.r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "   "}),
    )
    .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST);
    quick("an empty goal", t);
}

#[tokio::test]
async fn only_the_pcs_runner_gets_a_goals_start() {
    let h = hub().await;
    let s = setup(&h.dir);
    let app = add_app(&h, &s.folder).await;
    let g = ok(
        &h.r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "Add a page"}),
    )
    .await;
    let goal = g["id"].as_i64().expect("goal id");
    let path = format!("/api/runner/goals/{goal}/base");
    assert_eq!(
        send(&h.r, "GET", &path, OWNER, Value::Null).await.status,
        StatusCode::FORBIDDEN
    );
    // A cloud worker's key.
    let code = ok(&h.r, "POST", "/api/workers/join-code", OWNER, Value::Null).await;
    let joined = send(
        &h.r,
        "POST",
        "/runner/join",
        "",
        json!({"code": code["code"]}),
    )
    .await
    .json();
    let cloud = joined["token"].as_str().expect("token");
    assert_eq!(
        send(&h.r, "GET", &path, cloud, Value::Null).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&h.r, "GET", &path, RUNNER, Value::Null).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn helper_jobs_go_only_to_runners_on_the_hubs_rules() {
    let h = hub().await;
    let s = setup(&h.dir);
    let app = add_app(&h, &s.folder).await;
    ok(
        &h.r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "Add a page"}),
    )
    .await;
    for body in [
        json!({"runner": "local"}),
        json!({"runner": "local", "rules_version": decurion_proto::RULES_VERSION - 1}),
    ] {
        let resp = send(&h.r, "POST", "/api/runner/helper/claim", RUNNER, body).await;
        assert_eq!(resp.status, StatusCode::CONFLICT);
        assert_eq!(
            resp.json()["rules_version"],
            json!(decurion_proto::RULES_VERSION)
        );
    }
    let claim = ok(
        &h.r,
        "POST",
        "/api/runner/helper/claim",
        RUNNER,
        json!({"runner": "local", "rules_version": decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(claim["kind"], "coordinator");
}

/// Forward slashes: git reads them on every system, and a shell does too.
fn slashed(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

#[tokio::test]
async fn the_server_is_reached_the_way_the_owners_own_folder_reaches_it() {
    let h = hub().await;
    let s = setup(&h.dir);
    // The folder's address only works through its own settings for that
    // remote (as with a company mirror): here, how the server is started.
    let name = h.dir.join("mirror.git");
    git(&s.folder, &["remote", "set-url", "origin", &slashed(&name)]);
    git(
        &s.folder,
        &[
            "config",
            "remote.origin.uploadpack",
            &format!("git-upload-pack '{}' #", slashed(&s.origin)),
        ],
    );
    let app = add_app(&h, &s.folder).await;
    assert_eq!(
        start_of(&h, new_work(&h, app).await).await,
        s.b,
        "c = {}",
        s.c
    );
    let b2 = s.advance("b2");
    h.later();
    assert_eq!(start_of(&h, new_work(&h, app).await).await, b2);
}

#[tokio::test]
async fn asking_the_server_leaves_the_owners_folder_as_it_was() {
    let h = hub().await;
    let s = setup(&h.dir);
    // Settings that would have a plain fetch prune, take tags and update
    // the folder's branches.
    git(&s.folder, &["config", "fetch.prune", "true"]);
    git(&s.folder, &["config", "fetch.pruneTags", "true"]);
    git(&s.folder, &["tag", "mine"]);
    let app = add_app(&h, &s.folder).await;
    let refs = || {
        git(
            &s.folder,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname) %(symref)",
            ],
        )
    };
    let objects = || git(&s.folder, &["count-objects", "-v"]);
    let (refs_before, objects_before) = (refs(), objects());

    // The server has commits the folder has never seen, and ones it has.
    let b2 = s.advance("b2");
    assert_eq!(start_of(&h, new_work(&h, app).await).await, b2);
    assert_eq!(refs(), refs_before);
    assert_eq!(objects(), objects_before);
    assert!(!s.folder.join(".git").join("FETCH_HEAD").exists());
}

#[tokio::test]
async fn a_fallback_only_ever_pins_a_commit_the_hub_holds() {
    let h = hub().await;
    let s = setup(&h.dir);
    let app = add_app(&h, &s.folder).await;
    // A commit in the folder that no branch holds yet...
    let x = git(&s.folder, &["commit-tree", "HEAD^{tree}", "-m", "x"]);
    // ...becomes its main while the hub waits on a server that then fails.
    git(
        &s.folder,
        &[
            "config",
            "remote.origin.uploadpack",
            &format!(
                "git --git-dir='{}' update-ref refs/heads/main {x}; false",
                slashed(&s.folder.join(".git"))
            ),
        ],
    );
    assert_eq!(start_of(&h, new_work(&h, app).await).await, x);
}

#[tokio::test]
async fn a_helper_gets_the_start_it_was_claimed_with() {
    let h = hub().await;
    let s = setup(&h.dir);
    let app = add_app(&h, &s.folder).await;
    let g = ok(
        &h.r,
        "POST",
        "/api/goals",
        OWNER,
        json!({"app_id": app, "text": "Add a page"}),
    )
    .await;
    let goal = g["id"].as_i64().expect("goal id");
    // A later round moves the goal's start between a helper job's claim
    // (at b) and its download.
    let b2 = s.advance("b2");
    h.later();
    assert_eq!(start_of(&h, new_work(&h, app).await).await, b2);
    sqlx::query("UPDATE goals SET start_commit = ? WHERE id = ?")
        .bind(&b2)
        .bind(goal)
        .execute(h.store.pool())
        .await
        .expect("move the start");
    let at = |sha: &str| format!("/api/runner/goals/{goal}/base?at={sha}");
    for want in [&s.b, &b2] {
        let resp = send(&h.r, "GET", &at(want), RUNNER, Value::Null).await;
        assert_eq!(resp.status, StatusCode::OK, "{}", resp.json());
        assert_eq!(resp.headers["x-decurion-base"], want.as_str());
    }
    // Not a start this goal or app ever had, or not a commit id at all.
    assert_eq!(
        send(&h.r, "GET", &at(&"0".repeat(40)), RUNNER, Value::Null)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&h.r, "GET", &at("HEAD"), RUNNER, Value::Null)
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}
