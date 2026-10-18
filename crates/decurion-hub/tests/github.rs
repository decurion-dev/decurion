//! Accepted work goes to GitHub: the hub pushes it from the app's own
//! folder to a new branch on its `origin` and opens a pull request, and
//! never touches the default branch, pushes twice or merges. `origin` is a
//! local bare repo and GitHub's API a small fake on this computer, so no
//! test reaches the network. The runner's side, real git included, is
//! played by the test.

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::routing::get;
use axum::Json;
use decurion_hub::github::{GitHub, Tokens};
use decurion_hub::{router, store::Store, AppState, Keys};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";
const TOKEN: &str = "ghp_testtokenabcdefghijklmnopqrstuvwxyz01";
/// What the app's folder says its GitHub address is. Pushes go to the local
/// bare repo instead (`pushInsteadOf`), as a real remote's would go to
/// GitHub.
const GITHUB_URL: &str = "https://github.com/acme/demo.git";

// ---------- A fake GitHub API ----------

#[derive(Default)]
struct Fake {
    /// Every request: method, path, the Authorization header and the body.
    seen: Vec<(String, String, String, Value)>,
    /// Pull requests opened: head branch and page.
    prs: Vec<(String, String)>,
    /// Pull requests merged on GitHub, by number.
    merged: Vec<i64>,
    /// What GitHub says about merging each pull request ("clean" when left
    /// out) and the commit at its head, by number.
    merge_state: HashMap<i64, String>,
    heads: HashMap<i64, String>,
    /// The commit GitHub made on the base branch for each merged one.
    merge_commits: HashMap<i64, String>,
    /// The branch each pull request merges into ("main" when left out).
    bases: HashMap<i64, String>,
    /// Comments on lines of code, plain comments and reviews, as GitHub
    /// lists them.
    line: Vec<Value>,
    issue: Vec<Value>,
    reviews: Vec<Value>,
    /// What GitHub answers when asked to open a pull request, when not 201:
    /// 403 is a token that may not open them on this repo.
    refuse_open: Option<u16>,
}

type FakeState = Arc<Mutex<Fake>>;

fn auth(h: &HeaderMap) -> String {
    h.get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned()
}

async fn fake_list(
    State(f): State<FakeState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Json<Value> {
    let mut f = f.lock().expect("fake");
    f.seen
        .push(("GET".into(), "pulls".into(), auth(&headers), json!(q)));
    let head = q.get("head").cloned().unwrap_or_default();
    let list: Vec<Value> = f
        .prs
        .iter()
        .filter(|(h, _)| format!("acme:{h}") == head)
        .map(|(_, u)| json!({ "html_url": u, "number": u.rsplit('/').next().and_then(|n| n.parse::<i64>().ok()) }))
        .collect();
    Json(Value::Array(list))
}

async fn fake_open(
    State(f): State<FakeState>,
    headers: HeaderMap,
    Json(b): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let mut f = f.lock().expect("fake");
    f.seen
        .push(("POST".into(), "pulls".into(), auth(&headers), b.clone()));
    if let Some(code) = f.refuse_open {
        return (
            StatusCode::from_u16(code).expect("status"),
            Json(json!({"message": "Resource not accessible by personal access token"})),
        );
    }
    let url = format!("https://github.com/acme/demo/pull/{}", f.prs.len() + 1);
    f.prs.push((
        b["head"].as_str().unwrap_or_default().to_owned(),
        url.clone(),
    ));
    (
        StatusCode::CREATED,
        Json(json!({ "html_url": url, "number": f.prs.len() })),
    )
}

/// The owner's login, as the fake says who opened each pull request.
const ME: &str = "owner-me";

async fn fake_pr(
    State(f): State<FakeState>,
    headers: HeaderMap,
    axum::extract::Path(n): axum::extract::Path<i64>,
) -> Json<Value> {
    let mut f = f.lock().expect("fake");
    f.seen.push((
        "GET".into(),
        format!("pulls/{n}"),
        auth(&headers),
        Value::Null,
    ));
    let merged = f.merged.contains(&n);
    let state = f.merge_state.get(&n).cloned().unwrap_or("clean".into());
    Json(json!({
        "number": n,
        "html_url": format!("https://github.com/acme/demo/pull/{n}"),
        "state": if merged { "closed" } else { "open" },
        "merged": merged,
        "user": { "login": ME },
        "head": { "sha": f.heads.get(&n) },
        "merge_commit_sha": f.merge_commits.get(&n),
        "base": { "ref": f.bases.get(&n).cloned().unwrap_or("main".into()) },
        "mergeable": if state == "unknown" { Value::Null } else { json!(true) },
        "mergeable_state": state,
    }))
}

async fn fake_merge(
    State(f): State<FakeState>,
    headers: HeaderMap,
    axum::extract::Path(n): axum::extract::Path<i64>,
    Json(b): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let mut f = f.lock().expect("fake");
    f.seen.push((
        "PUT".into(),
        format!("merge/{n}"),
        auth(&headers),
        b.clone(),
    ));
    if f.heads.get(&n).map(String::as_str) != b["sha"].as_str() {
        return (
            StatusCode::CONFLICT,
            Json(json!({"message": "Head branch was modified"})),
        );
    }
    f.merged.push(n);
    (StatusCode::OK, Json(json!({ "merged": true })))
}

async fn fake_comments(
    State(f): State<FakeState>,
    headers: HeaderMap,
    axum::extract::Path((kind, n, list)): axum::extract::Path<(String, i64, String)>,
) -> Json<Value> {
    let mut f = f.lock().expect("fake");
    f.seen.push((
        "GET".into(),
        format!("{kind}/{n}/{list}"),
        auth(&headers),
        Value::Null,
    ));
    Json(Value::Array(match (kind.as_str(), list.as_str()) {
        ("pulls", "comments") => f.line.clone(),
        ("issues", "comments") => f.issue.clone(),
        ("pulls", "reviews") => f.reviews.clone(),
        _ => Vec::new(),
    }))
}

async fn fake_ask_review(
    State(f): State<FakeState>,
    headers: HeaderMap,
    axum::extract::Path(n): axum::extract::Path<i64>,
    Json(b): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let mut f = f.lock().expect("fake");
    f.seen
        .push(("POST".into(), format!("reviewers/{n}"), auth(&headers), b));
    (StatusCode::CREATED, Json(json!({ "number": n })))
}

async fn fake_repo(State(f): State<FakeState>, headers: HeaderMap) -> Json<Value> {
    let mut f = f.lock().expect("fake");
    f.seen
        .push(("GET".into(), "repo".into(), auth(&headers), Value::Null));
    Json(json!({ "default_branch": "main" }))
}

/// Starts the fake API and returns its address.
async fn fake_github(f: FakeState) -> String {
    let app = axum::Router::new()
        .route("/repos/acme/demo/pulls", get(fake_list).post(fake_open))
        .route("/repos/acme/demo", get(fake_repo))
        .route("/repos/acme/demo/pulls/{n}", get(fake_pr))
        .route(
            "/repos/acme/demo/pulls/{n}/merge",
            axum::routing::put(fake_merge),
        )
        .route(
            "/repos/acme/demo/pulls/{n}/requested_reviewers",
            axum::routing::post(fake_ask_review),
        )
        .route("/repos/acme/demo/{kind}/{n}/{list}", get(fake_comments))
        .with_state(f);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = l.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(l, app).await });
    format!("http://{addr}")
}

// ---------- The hub ----------

struct Hub {
    r: axum::Router,
    state: AppState,
    store: Store,
    dir: PathBuf,
    fake: FakeState,
    /// The fake API's address.
    api: String,
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

enum Tok {
    None,
    Fixed,
    /// The owner's token as saved in Settings.
    Settings,
}

async fn hub(tok: Tok) -> Hub {
    let dir = std::env::temp_dir().join(format!("decurion-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let fake = FakeState::default();
    let api = fake_github(fake.clone()).await;
    let vault = decurion_vault::Vault::in_folder(dir.join("vault"));
    let tokens = match tok {
        Tok::None => Tokens::None,
        Tok::Fixed => Tokens::Fixed(TOKEN.into()),
        Tok::Settings => Tokens::Owner(Some(vault.clone())),
    };
    let state = AppState::new(
        store.clone(),
        Keys {
            owner: OWNER.into(),
            runner: RUNNER.into(),
        },
        "abc123".into(),
        None,
    )
    .with_files(dir.join("saved"))
    .with_vault(vault)
    .with_github(GitHub::new(&api, tokens).expect("github"));
    Hub {
        r: router(state.clone()),
        state,
        store,
        dir,
        fake,
        api,
    }
}

async fn send(
    r: &axum::Router,
    key: &str,
    method: &str,
    path: &str,
    body: Body,
) -> (StatusCode, Vec<u8>, HeaderMap) {
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
    let (s, bytes, _) = send(r, key, method, path, body).await;
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (s, v)
}

async fn ok(r: &axum::Router, key: &str, method: &str, path: &str, body: Value) -> Value {
    let (s, v) = call(r, key, method, path, body).await;
    assert!(s.is_success(), "{method} {path}: {s} {v}");
    v
}

// ---------- Git ----------

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

/// How the app's folder is linked to its remote.
enum Origin {
    /// No remote at all.
    None,
    /// A plain git server (here, the bare repo's own path).
    Plain,
    /// GitHub, as far as the folder says; pushes land in the bare repo.
    GitHub,
}

/// The owner's app checkout and its `origin` bare repo (which keeps a log
/// of every change to its branches, so a test can count pushes).
fn app_repo(dir: &Path, origin: Origin) -> (PathBuf, PathBuf) {
    let repo = dir.join("app");
    let bare = dir.join("origin.git");
    std::fs::create_dir_all(&repo).expect("repo dir");
    git(&repo, &["init", "--quiet", "-b", "main"]);
    write(&repo, "README.md", "hello\n");
    write(&repo, "src/app.rs", "fn main() {}\n");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "first"]);
    std::fs::create_dir_all(&bare).expect("bare dir");
    git(&bare, &["init", "--quiet", "--bare", "-b", "main"]);
    git(&bare, &["config", "core.logAllRefUpdates", "always"]);
    git(&repo, &["push", "--quiet", &bare.to_string_lossy(), "main"]);
    link(&repo, &bare, origin);
    (repo, bare)
}

fn link(repo: &Path, bare: &Path, origin: Origin) {
    let bare_s = bare.to_string_lossy().into_owned();
    match origin {
        Origin::None => {}
        Origin::Plain => {
            git(repo, &["remote", "add", "origin", &bare_s]);
        }
        Origin::GitHub => {
            git(repo, &["remote", "add", "origin", GITHUB_URL]);
            git(
                repo,
                &["config", &format!("url.{bare_s}.pushInsteadOf"), GITHUB_URL],
            );
            git(repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
            git(
                repo,
                &[
                    "symbolic-ref",
                    "refs/remotes/origin/HEAD",
                    "refs/remotes/origin/main",
                ],
            );
        }
    }
}

/// Adds an app and a piece of work, builds it the way a runner does (from
/// the hub's starting point, one commit, saved as a checkpoint) and puts it
/// in review. Returns the thread and the work's commit.
async fn reviewed_work(h: &Hub, repo: &Path) -> (i64, String) {
    let app = ok(
        &h.r,
        OWNER,
        "POST",
        "/api/apps",
        json!({"name": "demo", "repo_path": repo.to_string_lossy(), "profile": "A"}),
    )
    .await["id"]
        .as_i64()
        .expect("app id");
    reviewed_work_in(h, app).await
}

/// Like [`reviewed_work`], for an app already added.
async fn reviewed_work_in(h: &Hub, app: i64) -> (i64, String) {
    ok(
        &h.r,
        OWNER,
        "POST",
        "/api/threads",
        json!({"app_id": app, "title": "Fix the login page", "prompt": "The login page is broken."}),
    )
    .await;
    let work = ok(
        &h.r,
        RUNNER,
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
    let head = save_work(h, thread, &branch).await;
    ok(
        &h.r,
        RUNNER,
        "POST",
        &format!("/api/threads/{thread}/events"),
        json!({"kind":"finished","ok":true,"summary":"Fixed the login form."}),
    )
    .await;
    ok(
        &h.r,
        RUNNER,
        "POST",
        &format!("/api/threads/{thread}/move"),
        json!({"to":"in_review"}),
    )
    .await;
    (thread, head)
}

async fn save_work(h: &Hub, thread: i64, branch: &str) -> String {
    save_work_with(h, thread, branch, "src/login.rs", "fn login() {}\n").await
}

/// Like [`save_work`], with one file of the test's choosing.
async fn save_work_with(h: &Hub, thread: i64, branch: &str, file: &str, text: &str) -> String {
    let (s, bundle, headers) = send(
        &h.r,
        RUNNER,
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
    write(&wt, file, text);
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "--quiet", "-m", "Fix the login page"]);
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
    for (part, data) in [
        ("code", std::fs::read(&code).expect("read")),
        ("session", b"SESSION TAR".to_vec()),
    ] {
        let (s, _, _) = send(
            &h.r,
            RUNNER,
            "PUT",
            &format!("/api/runner/threads/{thread}/upload/{part}"),
            Body::from(data),
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT);
    }
    ok(
        &h.r,
        RUNNER,
        "POST",
        &format!("/api/runner/threads/{thread}/checkpoint"),
        json!({"head": head, "base": base}),
    )
    .await;
    head
}

async fn accept(h: &Hub, thread: i64, receipt: &str) -> Value {
    ok(
        &h.r,
        OWNER,
        "POST",
        &format!("/api/threads/{thread}/move"),
        json!({"to":"done","expect":"in_review","receipt":receipt}),
    )
    .await
}

/// Waits for the hub's send to end, and returns the thread.
async fn settled(h: &Hub, thread: i64) -> Value {
    for _ in 0..400 {
        let t = ok(
            &h.r,
            OWNER,
            "GET",
            &format!("/api/threads/{thread}"),
            Value::Null,
        )
        .await;
        if matches!(t["github"]["state"].as_str(), Some("sent" | "failed")) {
            return t;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the send to GitHub never ended");
}

/// The commit a branch of the bare repo points at, if it is there.
fn branch_at(bare: &Path, branch: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(bare)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .expect("git");
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// How many times a branch of the bare repo was changed.
fn updates(bare: &Path, branch: &str) -> usize {
    git(
        bare,
        &[
            "reflog",
            "show",
            "--format=%H",
            &format!("refs/heads/{branch}"),
        ],
    )
    .lines()
    .count()
}

/// Every request the fake GitHub API got so far.
fn requests(h: &Hub) -> Vec<(String, String, String, Value)> {
    h.fake.lock().expect("fake").seen.clone()
}

async fn hub_audits(h: &Hub, thread: i64) -> Vec<String> {
    sqlx::query_scalar("SELECT note FROM audit WHERE thread_id = ? AND actor = 'hub' ORDER BY id")
        .bind(thread)
        .fetch_all(h.store.pool())
        .await
        .expect("audit")
}

// ---------- Tests ----------

#[tokio::test]
async fn accepting_work_sends_it_to_github_as_a_branch_and_a_pull_request() {
    let h = hub(Tok::Fixed).await;
    let (repo, bare) = app_repo(&h.dir, Origin::GitHub);
    let main_before = branch_at(&bare, "main").expect("main");
    let (thread, head) = reviewed_work(&h, &repo).await;
    let t = accept(&h, thread, "accept-receipt-1").await;
    assert_eq!(t["state"], "done");
    let branch = format!("decurion/demo-{thread}-fix-the-login-page");
    assert_eq!(t["github"]["branch"], branch.as_str());
    // The commit to send is fixed with the accept itself.
    let fixed: Option<String> = sqlx::query_scalar("SELECT gh_head FROM threads WHERE id = ?")
        .bind(thread)
        .fetch_one(h.store.pool())
        .await
        .expect("head");
    assert_eq!(fixed.as_deref(), Some(head.as_str()));

    let t = settled(&h, thread).await;
    assert_eq!(t["state"], "done");
    let g = &t["github"];
    assert_eq!(g["state"], "sent", "{g}");
    assert_eq!(g["pull_request"], true);
    assert_eq!(g["url"], "https://github.com/acme/demo/pull/1");
    assert!(g["error"].is_null(), "{g}");

    // The branch holds exactly the work; the default branch is untouched.
    assert_eq!(branch_at(&bare, &branch).as_deref(), Some(head.as_str()));
    assert_eq!(branch_at(&bare, "main"), Some(main_before));
    // The owner's own folder got no new branches or commits.
    assert_eq!(git(&repo, &["branch", "--list", "decurion/*"]), "");
    assert!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["cat-file", "-e", &head])
            .output()
            .expect("git")
            .status
            .code()
            != Some(0)
    );

    // One pull request, from the branch to main, with plain words, and the
    // token only ever in the request header.
    let opened: Vec<_> = requests(&h).into_iter().filter(|s| s.0 == "POST").collect();
    assert_eq!(opened.len(), 1);
    let (_, _, auth, body) = &opened[0];
    assert_eq!(auth, &format!("Bearer {TOKEN}"));
    assert_eq!(body["head"], branch.as_str());
    assert_eq!(body["base"], "main");
    assert_eq!(body["title"], "Fix the login page");
    let text = body["body"].as_str().expect("body");
    assert!(text.contains("Fixed the login form."), "{text}");
    assert!(text.contains("Decurion made this pull request"), "{text}");
    assert!(!text.contains(TOKEN));

    let notes = hub_audits(&h, thread).await;
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("https://github.com/acme/demo/pull/1"),
        "{notes:?}"
    );
    // Nothing saved for the board holds the token.
    let all: Vec<String> = sqlx::query_scalar(
        "SELECT COALESCE(gh_url, '') || COALESCE(gh_error, '') || COALESCE(gh_branch, '') FROM threads",
    )
    .fetch_all(h.store.pool())
    .await
    .expect("rows");
    assert!(all.iter().all(|r| !r.contains(TOKEN)));
}

#[tokio::test]
async fn without_a_token_the_owner_gets_a_link_that_opens_the_pull_request() {
    let h = hub(Tok::None).await;
    let (repo, bare) = app_repo(&h.dir, Origin::GitHub);
    let (thread, head) = reviewed_work(&h, &repo).await;
    accept(&h, thread, "accept-receipt-2").await;
    let t = settled(&h, thread).await;
    let branch = format!("decurion/demo-{thread}-fix-the-login-page");
    assert_eq!(t["github"]["state"], "sent", "{t}");
    assert_eq!(t["github"]["pull_request"], false);
    assert_eq!(
        t["github"]["url"],
        format!("https://github.com/acme/demo/compare/main...{branch}?expand=1").as_str()
    );
    assert_eq!(branch_at(&bare, &branch), Some(head));
    assert!(h.fake.lock().expect("fake").seen.is_empty());
}

#[tokio::test]
async fn a_resent_accept_or_a_restart_never_pushes_or_opens_a_pull_request_twice() {
    let h = hub(Tok::Fixed).await;
    let (repo, bare) = app_repo(&h.dir, Origin::GitHub);
    let (thread, head) = reviewed_work(&h, &repo).await;
    accept(&h, thread, "accept-receipt-3").await;
    // The same tap sent again (a dropped connection) changes nothing.
    accept(&h, thread, "accept-receipt-3").await;
    let first = settled(&h, thread).await;
    let branch = format!("decurion/demo-{thread}-fix-the-login-page");
    assert_eq!(updates(&bare, &branch), 1);

    // The hub stops halfway through a send: after the push and the pull
    // request, before it could record them.
    sqlx::query("UPDATE threads SET gh_state = 'sending', gh_url = NULL, gh_pr = 0 WHERE id = ?")
        .bind(thread)
        .execute(h.store.pool())
        .await
        .expect("cut off");
    assert_eq!(decurion_hub::github::resume(&h.state).await, 1);
    let again = settled(&h, thread).await;
    assert_eq!(again["github"], first["github"]);
    // Still one change to the branch, the same commit, one pull request.
    assert_eq!(updates(&bare, &branch), 1);
    assert_eq!(branch_at(&bare, &branch), Some(head));
    assert_eq!(requests(&h).iter().filter(|s| s.0 == "POST").count(), 1);
    assert_eq!(h.fake.lock().expect("fake").prs.len(), 1);
    // A start with nothing waiting does nothing.
    assert_eq!(decurion_hub::github::resume(&h.state).await, 0);
    let refs = git(&bare, &["for-each-ref", "--format=%(refname)"]);
    assert_eq!(refs.lines().count(), 2, "{refs}");
}

#[tokio::test]
async fn a_failed_send_keeps_the_work_done_with_a_reason_and_try_again_sends_it() {
    let h = hub(Tok::Fixed).await;
    let (repo, bare) = app_repo(&h.dir, Origin::None);
    let (thread, head) = reviewed_work(&h, &repo).await;
    accept(&h, thread, "accept-receipt-4").await;
    let t = settled(&h, thread).await;
    assert_eq!(t["state"], "done");
    assert_eq!(t["github"]["state"], "failed");
    let why = t["github"]["error"].as_str().expect("reason");
    assert!(why.contains("not linked to GitHub"), "{why}");
    assert!(why.contains("by hand"), "{why}");
    assert!(hub_audits(&h, thread).await[0].starts_with("could not send to GitHub"));

    // Only the owner can ask again.
    let path = format!("/api/threads/{thread}/github");
    let (s, _) = call(
        &h.r,
        RUNNER,
        "POST",
        &path,
        json!({"receipt":"retry-receipt-1"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // The owner links the folder to its server and taps Try again.
    link(&repo, &bare, Origin::Plain);
    let t = ok(
        &h.r,
        OWNER,
        "POST",
        &path,
        json!({"receipt":"retry-receipt-1"}),
    )
    .await;
    assert!(
        matches!(
            t["github"]["state"].as_str(),
            Some("waiting" | "sending" | "sent")
        ),
        "{t}"
    );
    let t = settled(&h, thread).await;
    assert_eq!(t["github"]["state"], "sent", "{t}");
    // Not GitHub, so there is no pull request or link, only the branch.
    assert!(t["github"]["url"].is_null());
    let branch = format!("decurion/demo-{thread}-fix-the-login-page");
    assert_eq!(branch_at(&bare, &branch), Some(head));

    // Sent work is not sent again.
    let (s, v) = call(
        &h.r,
        OWNER,
        "POST",
        &path,
        json!({"receipt":"retry-receipt-2"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    // The same tap resent gets the first answer, not an error.
    let (s, _) = call(
        &h.r,
        OWNER,
        "POST",
        &path,
        json!({"receipt":"retry-receipt-1"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn a_branch_already_there_with_other_changes_is_never_overwritten() {
    let h = hub(Tok::Fixed).await;
    let (repo, bare) = app_repo(&h.dir, Origin::GitHub);
    let (thread, _) = reviewed_work(&h, &repo).await;
    // Someone already made a branch of that name, with other history.
    let branch = format!("decurion/demo-{thread}-fix-the-login-page");
    let other = h.dir.join("other");
    git(
        &h.dir,
        &[
            "clone",
            "--quiet",
            &bare.to_string_lossy(),
            &other.to_string_lossy(),
        ],
    );
    git(&other, &["checkout", "--quiet", "-b", &branch]);
    write(&other, "theirs.txt", "not yours\n");
    git(&other, &["add", "."]);
    git(&other, &["commit", "--quiet", "-m", "theirs"]);
    git(&other, &["push", "--quiet", "origin", &branch]);
    let theirs = branch_at(&bare, &branch).expect("their branch");

    accept(&h, thread, "accept-receipt-5").await;
    let t = settled(&h, thread).await;
    assert_eq!(t["state"], "done");
    assert_eq!(t["github"]["state"], "failed");
    let why = t["github"]["error"].as_str().expect("reason");
    assert!(
        why.contains("already on GitHub with other changes"),
        "{why}"
    );
    assert_eq!(branch_at(&bare, &branch), Some(theirs));
    assert!(h.fake.lock().expect("fake").seen.is_empty());
}

#[tokio::test]
async fn work_never_goes_to_the_default_branch_whatever_the_branch_says() {
    let h = hub(Tok::Fixed).await;
    let (repo, bare) = app_repo(&h.dir, Origin::GitHub);
    let main_before = branch_at(&bare, "main").expect("main");
    let (thread, _) = reviewed_work(&h, &repo).await;
    accept(&h, thread, "accept-receipt-6").await;
    settled(&h, thread).await;
    // However the stored name came to be, a send to main or to anything
    // outside decurion/ is refused before git runs.
    for name in ["main", "refs/heads/main", "feature/x", "decurion/../main"] {
        sqlx::query(
            "UPDATE threads SET gh_state = 'waiting', gh_branch = ?, gh_url = NULL WHERE id = ?",
        )
        .bind(name)
        .bind(thread)
        .execute(h.store.pool())
        .await
        .expect("set branch");
        decurion_hub::github::resume(&h.state).await;
        let t = settled(&h, thread).await;
        assert_eq!(t["github"]["state"], "failed", "{name}: {t}");
        assert_eq!(
            branch_at(&bare, "main").as_deref(),
            Some(main_before.as_str())
        );
    }
    assert!(branch_at(&bare, "feature/x").is_none());
}

#[tokio::test]
async fn only_the_commit_the_owner_accepted_is_ever_sent() {
    let h = hub(Tok::Fixed).await;
    let (repo, bare) = app_repo(&h.dir, Origin::GitHub);
    let (thread, _) = reviewed_work(&h, &repo).await;
    // The hub's record of what was accepted and the saved copy disagree.
    sqlx::query("UPDATE checkpoints SET head = ? WHERE thread_id = ?")
        .bind("b".repeat(40))
        .bind(thread)
        .execute(h.store.pool())
        .await
        .expect("change record");
    accept(&h, thread, "accept-receipt-8").await;
    let t = settled(&h, thread).await;
    assert_eq!(t["state"], "done");
    assert_eq!(t["github"]["state"], "failed", "{t}");
    let refs = git(&bare, &["for-each-ref", "--format=%(refname)"]);
    assert_eq!(refs, "refs/heads/main");
}

#[tokio::test]
async fn work_rejected_or_sent_back_is_not_sent() {
    let h = hub(Tok::Fixed).await;
    let (repo, bare) = app_repo(&h.dir, Origin::GitHub);
    let (thread, _) = reviewed_work(&h, &repo).await;
    ok(
        &h.r,
        OWNER,
        "POST",
        &format!("/api/threads/{thread}/move"),
        json!({"to":"failed","expect":"in_review","receipt":"reject-receipt-1"}),
    )
    .await;
    assert_eq!(decurion_hub::github::resume(&h.state).await, 0);
    let t = ok(
        &h.r,
        OWNER,
        "GET",
        &format!("/api/threads/{thread}"),
        Value::Null,
    )
    .await;
    assert!(t["github"].is_null(), "{t}");
    let refs = git(&bare, &["for-each-ref", "--format=%(refname)"]);
    assert_eq!(refs, "refs/heads/main");
}

#[tokio::test]
async fn the_owner_cannot_skip_the_finish_checks_on_the_way_to_github() {
    // A runner moves work to review only after checking its branch for test
    // keys; accepted work then goes to GitHub. The owner's own tap on
    // running work must not get it there without that check.
    let h = hub(Tok::Fixed).await;
    let (repo, bare) = app_repo(&h.dir, Origin::GitHub);
    let app = ok(
        &h.r,
        OWNER,
        "POST",
        "/api/apps",
        json!({"name": "demo", "repo_path": repo.to_string_lossy(), "profile": "A"}),
    )
    .await["id"]
        .as_i64()
        .expect("app id");
    ok(
        &h.r,
        OWNER,
        "POST",
        "/api/threads",
        json!({"app_id": app, "title": "Half done", "prompt": "Do it."}),
    )
    .await;
    let work = ok(
        &h.r,
        RUNNER,
        "POST",
        "/api/runner/claim",
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    let thread = work["thread"]["id"].as_i64().expect("thread");
    let (s, v) = call(
        &h.r,
        OWNER,
        "POST",
        &format!("/api/threads/{thread}/move"),
        json!({"to":"in_review","expect":"running"}),
    )
    .await;
    assert!(s.is_client_error(), "{s} {v}");
    assert!(v.to_string().contains("stop it instead"), "{v}");
    let t = ok(
        &h.r,
        OWNER,
        "GET",
        &format!("/api/threads/{thread}"),
        Value::Null,
    )
    .await;
    assert_eq!(t["state"], "running", "{t}");
    assert_eq!(decurion_hub::github::resume(&h.state).await, 0);
    let refs = git(&bare, &["for-each-ref", "--format=%(refname)"]);
    assert_eq!(refs, "refs/heads/main");
}

#[tokio::test]
async fn the_github_token_is_saved_once_and_shown_only_by_its_last_four() {
    let h = hub(Tok::Settings).await;
    let (s, v) = call(&h.r, OWNER, "GET", "/api/github", Value::Null).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["token"].is_null(), "{v}");

    let bad = "hunter2hunter2hunter2hunter2";
    let (s, v) = call(
        &h.r,
        OWNER,
        "POST",
        "/api/github/token",
        json!({"value": bad}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(!v.to_string().contains(bad), "{v}");
    for (method, path) in [
        ("GET", "/api/github"),
        ("POST", "/api/github/token"),
        ("POST", "/api/github/token/remove"),
    ] {
        let (s, _) = call(&h.r, RUNNER, method, path, json!({"value": TOKEN})).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{path}");
    }

    let (s, _) = call(
        &h.r,
        OWNER,
        "POST",
        "/api/github/token",
        json!({"value": TOKEN}),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let v = ok(&h.r, OWNER, "GET", "/api/github", Value::Null).await;
    assert_eq!(v["token"]["last_four"], "yz01");
    assert!(!v.to_string().contains(TOKEN));

    // The saved token is what opens the pull request.
    let (repo, _) = app_repo(&h.dir, Origin::GitHub);
    let (thread, _) = reviewed_work(&h, &repo).await;
    accept(&h, thread, "accept-receipt-7").await;
    let t = settled(&h, thread).await;
    assert_eq!(t["github"]["pull_request"], true, "{t}");
    let seen = requests(&h);
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|s| s.2 == format!("Bearer {TOKEN}")));

    let (s, _) = call(&h.r, OWNER, "POST", "/api/github/token/remove", Value::Null).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let v = ok(&h.r, OWNER, "GET", "/api/github", Value::Null).await;
    assert!(v["token"].is_null(), "{v}");
}

#[tokio::test]
async fn cloud_workers_cannot_send_work_to_github() {
    let h = hub(Tok::Fixed).await;
    // Even with a key, a cloud worker reaches only its own running work.
    let code = ok(&h.r, OWNER, "POST", "/api/workers/join-code", Value::Null).await["code"]
        .as_str()
        .expect("code")
        .to_owned();
    let joined = ok(&h.r, OWNER, "POST", "/runner/join", json!({"code": code})).await;
    let cloud = joined["token"].as_str().expect("token").to_owned();
    for (method, path) in [
        ("POST", "/api/threads/1/github"),
        ("GET", "/api/github"),
        ("POST", "/api/github/token"),
    ] {
        let (s, _) = call(&h.r, &cloud, method, path, json!({})).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{path}");
    }
}

// ---------- Reviewers and watching pull requests ----------

/// An app whose work was accepted and sent as pull request 1, with `param`
/// as its reviewer. Returns the app folder, its remote, the work, its
/// commit and its branch.
async fn sent_pr(h: &Hub) -> (PathBuf, PathBuf, i64, String, String) {
    let (repo, bare) = app_repo(&h.dir, Origin::GitHub);
    let (thread, head) = reviewed_work(h, &repo).await;
    sqlx::query("UPDATE apps SET reviewer = 'param'")
        .execute(h.store.pool())
        .await
        .expect("reviewer");
    accept(h, thread, "accept-receipt-pr").await;
    let t = settled(h, thread).await;
    assert_eq!(t["github"]["state"], "sent", "{t}");
    let branch = format!("decurion/demo-{thread}-fix-the-login-page");
    (repo, bare, thread, head, branch)
}

fn comment(id: i64, login: &str, body: &str) -> Value {
    json!({
        "id": id, "user": { "login": login }, "body": body,
        "created_at": format!("2026-10-18T0{id}:00:00Z"),
        "html_url": format!("https://github.com/acme/demo/pull/1#c{id}"),
    })
}

/// Param leaves a comment on a line and a review; the owner answers, and
/// an approval says nothing.
fn param_comments(h: &Hub) {
    let mut f = h.fake.lock().expect("fake");
    let mut line = comment(1, "param", "Call this check_login, please.");
    line["path"] = json!("src/login.rs");
    line["line"] = json!(1);
    f.line.push(line);
    f.issue
        .push(comment(2, ME, "Thanks Param, Decurion will fix it."));
    let mut review = comment(3, "param", "Also add a test.");
    review["submitted_at"] = json!("2026-10-18T03:00:00Z");
    f.reviews.push(review);
    f.reviews.push(comment(4, "param", ""));
}

async fn watched(h: &Hub) -> Value {
    ok(&h.r, OWNER, "GET", "/api/prs", Value::Null).await
}

#[tokio::test]
async fn the_reviewer_is_asked_to_review_and_named_in_the_pull_request() {
    let h = hub(Tok::Fixed).await;
    let (_repo, _bare, thread, head, branch) = sent_pr(&h).await;
    let seen = requests(&h);
    let opened = seen
        .iter()
        .find(|s| s.0 == "POST" && s.1 == "pulls")
        .expect("opened");
    let text = opened.3["body"].as_str().expect("body");
    assert!(text.contains("param reviews next."), "{text}");
    let asked: Vec<_> = seen.iter().filter(|s| s.1 == "reviewers/1").collect();
    assert_eq!(asked.len(), 1, "{seen:?}");
    assert_eq!(asked[0].3, json!({ "reviewers": ["param"] }));
    assert_eq!(asked[0].2, format!("Bearer {TOKEN}"));
    // The pull request is now watched from the commit that was sent.
    let w = watched(&h).await;
    assert_eq!(w[0]["thread_id"], thread);
    assert_eq!(w[0]["number"], 1);
    assert_eq!(w[0]["state"], "open");
    assert_eq!(w[0]["reviewer"], "param");
    let (whead, wbranch): (String, String) =
        sqlx::query_as("SELECT head, branch FROM pr_watches WHERE thread_id = ?")
            .bind(thread)
            .fetch_one(h.store.pool())
            .await
            .expect("watch");
    assert_eq!((whead, wbranch), (head, branch));
    // Only the owner sees them.
    let (s, _) = call(&h.r, RUNNER, "GET", "/api/prs", Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn decurion_watches_its_pull_request_until_it_is_merged() {
    let h = hub(Tok::Fixed).await;
    let (_repo, _bare, thread, _head, _branch) = sent_pr(&h).await;
    assert_eq!(decurion_hub::pr_watch::poll(&h.state).await, 1);
    let w = watched(&h).await;
    assert_eq!(w[0]["open_comments"], 0, "{w}");

    param_comments(&h);
    assert_eq!(decurion_hub::pr_watch::poll(&h.state).await, 1);
    let w = watched(&h).await;
    // The owner's own answer and the empty approval are not asks.
    assert_eq!(w[0]["open_comments"], 2, "{w}");
    assert_eq!(w[0]["line"], "param left 2 comments");
    assert_eq!(w[0]["event"], "param left 2 comments");
    // Looking again finds nothing new.
    decurion_hub::pr_watch::poll(&h.state).await;
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pr_comments")
        .fetch_one(h.store.pool())
        .await
        .expect("count");
    assert_eq!(n, 2);

    h.fake.lock().expect("fake").merged.push(1);
    assert_eq!(decurion_hub::pr_watch::poll(&h.state).await, 1);
    let w = watched(&h).await;
    assert_eq!(w[0]["state"], "merged");
    assert_eq!(w[0]["event"], "Merged on GitHub");
    assert_eq!(w[0]["open_comments"], 0);
    // Watching ends with the merge: GitHub is not asked again.
    let before = requests(&h).len();
    assert_eq!(decurion_hub::pr_watch::poll(&h.state).await, 0);
    assert_eq!(requests(&h).len(), before);
    // A fix is refused once it is merged.
    let (s, v) = call(
        &h.r,
        OWNER,
        "POST",
        &format!("/api/threads/{thread}/pr/fix"),
        json!({"receipt": "fix-receipt-merged"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(
        v.as_str().expect("text").contains("merged or closed"),
        "{v}"
    );
}

/// The watcher reads how the pull request stands against its base branch
/// from the pull request itself, and shows it only while GitHub said it
/// about the commit Decurion pushed.
#[tokio::test]
async fn the_watcher_says_when_a_pull_request_conflicts_or_falls_behind() {
    let h = hub(Tok::Fixed).await;
    let (_repo, _bare, _thread, head, _branch) = sent_pr(&h).await;
    let health = |w: &Value| w[0]["health"].clone();
    let set = |state: &str, at: Option<&str>| {
        let mut f = h.fake.lock().expect("fake");
        f.merge_state.insert(1, state.into());
        match at {
            Some(sha) => f.heads.insert(1, sha.into()),
            None => f.heads.remove(&1),
        };
    };
    set("dirty", Some(&head));
    decurion_hub::pr_watch::poll(&h.state).await;
    let w = watched(&h).await;
    assert_eq!(health(&w), "Conflicts with main", "{w}");
    assert_eq!(w[0]["updating"], false);
    let base: Option<String> = sqlx::query_scalar("SELECT base_ref FROM pr_watches")
        .fetch_one(h.store.pool())
        .await
        .expect("base");
    assert_eq!(base.as_deref(), Some("main"));

    set("behind", Some(&head));
    decurion_hub::pr_watch::poll(&h.state).await;
    assert_eq!(health(&watched(&h).await), "Behind main");

    // Said about some other commit than Decurion's: not shown.
    set("dirty", Some(&"f".repeat(40)));
    decurion_hub::pr_watch::poll(&h.state).await;
    assert_eq!(health(&watched(&h).await), Value::Null);

    // Still checking, then clean: nothing to say.
    for s in ["unknown", "clean", "unstable"] {
        set(s, Some(&head));
        decurion_hub::pr_watch::poll(&h.state).await;
        assert_eq!(health(&watched(&h).await), Value::Null, "{s}");
    }
}

#[tokio::test]
async fn without_a_token_nothing_is_watched() {
    let h = hub(Tok::Fixed).await;
    sent_pr(&h).await;
    sqlx::query("UPDATE pr_watches SET state = 'open'")
        .execute(h.store.pool())
        .await
        .expect("open");
    let h2_state = h
        .state
        .clone()
        .with_github(GitHub::new("http://127.0.0.1:9", Tokens::None).expect("github"));
    assert_eq!(decurion_hub::pr_watch::poll(&h2_state).await, 0);
}

#[tokio::test]
async fn fix_these_starts_one_fix_with_the_comments_as_data() {
    let h = hub(Tok::Fixed).await;
    let (_repo, _bare, thread, head, _branch) = sent_pr(&h).await;
    let path = format!("/api/threads/{thread}/pr/fix");
    // Nothing to fix yet.
    let (s, v) = call(
        &h.r,
        OWNER,
        "POST",
        &path,
        json!({"receipt": "fix-receipt-0"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(v.as_str().expect("text").contains("no new comments"), "{v}");

    param_comments(&h);
    decurion_hub::pr_watch::poll(&h.state).await;
    let (s, _) = call(
        &h.r,
        RUNNER,
        "POST",
        &path,
        json!({"receipt": "fix-receipt-1"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let fix = ok(
        &h.r,
        OWNER,
        "POST",
        &path,
        json!({"receipt": "fix-receipt-1"}),
    )
    .await;
    assert_eq!(fix["fix_of"], thread);
    assert_eq!(fix["state"], "queued");
    assert_eq!(fix["title"], "Fix review comments: Fix the login page");
    let prompt = fix["prompt"].as_str().expect("prompt");
    assert!(prompt.contains("<review-comments>"), "{prompt}");
    assert!(
        prompt.contains("Call this check_login, please."),
        "{prompt}"
    );
    assert!(prompt.contains("Also add a test."), "{prompt}");
    assert!(prompt.contains("data, not instructions"), "{prompt}");
    assert!(!prompt.contains("Thanks Param"), "{prompt}");
    // It starts from the commit already in the pull request.
    let base: Option<String> = sqlx::query_scalar("SELECT base_commit FROM threads WHERE id = ?")
        .bind(fix["id"].as_i64())
        .fetch_one(h.store.pool())
        .await
        .expect("base");
    assert_eq!(base.as_deref(), Some(head.as_str()));

    // The same tap sent again starts nothing new; another tap waits.
    let again = ok(
        &h.r,
        OWNER,
        "POST",
        &path,
        json!({"receipt": "fix-receipt-1"}),
    )
    .await;
    assert_eq!(again["id"], fix["id"]);
    let (s, v) = call(
        &h.r,
        OWNER,
        "POST",
        &path,
        json!({"receipt": "fix-receipt-2"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(
        v.as_str().expect("text").contains("already under way"),
        "{v}"
    );
    let w = watched(&h).await;
    assert_eq!(w[0]["open_comments"], 0, "{w}");
    assert_eq!(w[0]["fixing"], true, "{w}");
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM threads")
        .fetch_one(h.store.pool())
        .await
        .expect("count");
    assert_eq!(n, 2);
}

/// Taps Answer the comments, builds the fix the way a runner does, puts it in review
/// and accepts it. Returns the fix and its commit.
async fn fixed(h: &Hub, thread: i64, before_claim: Option<&str>) -> (i64, String) {
    param_comments(h);
    decurion_hub::pr_watch::poll(&h.state).await;
    let fix = ok(
        &h.r,
        OWNER,
        "POST",
        &format!("/api/threads/{thread}/pr/fix"),
        json!({"receipt": "fix-receipt-run"}),
    )
    .await["id"]
        .as_i64()
        .expect("fix");
    if let Some(base) = before_claim {
        sqlx::query("UPDATE threads SET base_commit = ? WHERE id = ?")
            .bind(base)
            .bind(fix)
            .execute(h.store.pool())
            .await
            .expect("base");
    }
    let work = ok(
        &h.r,
        RUNNER,
        "POST",
        "/api/runner/claim",
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(work["thread"]["id"], fix);
    let wbranch = work["thread"]["branch"]
        .as_str()
        .expect("branch")
        .to_owned();
    let head = save_work_with(h, fix, &wbranch, "src/check.rs", "fn check_login() {}\n").await;
    ok(
        &h.r,
        RUNNER,
        "POST",
        &format!("/api/threads/{fix}/events"),
        json!({"kind":"finished","ok":true,"summary":"Renamed it and added a test."}),
    )
    .await;
    ok(
        &h.r,
        RUNNER,
        "POST",
        &format!("/api/threads/{fix}/move"),
        json!({"to":"in_review"}),
    )
    .await;
    accept(h, fix, "accept-receipt-fix").await;
    (fix, head)
}

#[tokio::test]
async fn an_accepted_fix_moves_the_same_branch_forward_and_opens_no_new_pull_request() {
    let h = hub(Tok::Fixed).await;
    let (_repo, bare, thread, head, branch) = sent_pr(&h).await;
    let main_before = branch_at(&bare, "main");
    let (fix, fix_head) = fixed(&h, thread, None).await;
    let t = settled(&h, fix).await;
    assert_eq!(t["github"]["state"], "sent", "{t}");
    assert_eq!(t["github"]["branch"], branch.as_str());
    assert_eq!(t["github"]["url"], "https://github.com/acme/demo/pull/1");
    // A plain fast-forward on the same branch, on top of the sent work.
    assert_eq!(
        branch_at(&bare, &branch).as_deref(),
        Some(fix_head.as_str())
    );
    assert_eq!(updates(&bare, &branch), 2);
    assert_eq!(git(&bare, &["rev-parse", &format!("{fix_head}^")]), head);
    assert_eq!(branch_at(&bare, "main"), main_before);
    // Still one pull request; the reviewer is asked to look again.
    let seen = requests(&h);
    assert_eq!(
        seen.iter()
            .filter(|s| s.1 == "pulls" && s.0 == "POST")
            .count(),
        1
    );
    assert_eq!(seen.iter().filter(|s| s.1 == "reviewers/1").count(), 2);
    // The watch moves on to the fix.
    let w = watched(&h).await;
    assert_eq!(w[0]["event"], "Fix sent for 2 comments", "{w}");
    assert_eq!(w[0]["fixing"], false);
    let whead: String = sqlx::query_scalar("SELECT head FROM pr_watches WHERE thread_id = ?")
        .bind(thread)
        .fetch_one(h.store.pool())
        .await
        .expect("head");
    assert_eq!(whead, fix_head);
}

#[tokio::test]
async fn a_fix_that_does_not_build_on_the_pull_request_is_never_sent() {
    let h = hub(Tok::Fixed).await;
    let (repo, bare, thread, head, branch) = sent_pr(&h).await;
    // The fix is built on the app's main line instead of the sent work.
    let main = git(&repo, &["rev-parse", "HEAD"]);
    let (fix, _) = fixed(&h, thread, Some(&main)).await;
    let t = settled(&h, fix).await;
    assert_eq!(t["github"]["state"], "failed", "{t}");
    let why = t["github"]["error"].as_str().expect("why");
    assert!(why.contains("does not build on"), "{why}");
    assert_eq!(branch_at(&bare, &branch), Some(head));
    assert_eq!(updates(&bare, &branch), 1);
}

#[test]
fn a_fix_goes_only_to_its_own_pull_requests_branch() {
    use decurion_hub::github::check_fix_branch;
    assert!(check_fix_branch("decurion/shop-7-fix", "decurion/shop-7-fix", Some("main")).is_ok());
    assert!(check_fix_branch("decurion/shop-8", "decurion/shop-7-fix", Some("main")).is_err());
    assert!(check_fix_branch("main", "main", Some("main")).is_err());
    assert!(check_fix_branch("decurion/x", "decurion/x", Some("decurion/x")).is_err());
}

/// Runs one helper job the hub has queued, answering with `text`.
async fn run_helper(h: &Hub, kind: &str, text: &str) {
    let claim = ok(
        &h.r,
        RUNNER,
        "POST",
        "/api/runner/helper/claim",
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(claim["kind"], kind, "{claim}");
    let id = claim["job_id"].as_i64().expect("job id");
    let (s, _, _) = send(
        &h.r,
        RUNNER,
        "POST",
        &format!("/api/runner/helper/{id}/started"),
        Body::from("{}"),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _, _) = send(
        &h.r,
        RUNNER,
        "POST",
        &format!("/api/runner/helper/{id}/finish"),
        Body::from(json!({"ok": true, "text": text, "cost_usd": 0.01}).to_string()),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}

/// A goal for app `demo` whose one piece the owner approved, built and
/// ready for its check. `rules` are set on the app first. Returns the goal,
/// the piece's thread, its commit and `origin`'s bare repo with its main.
async fn approved_piece(h: &Hub, rules: Value) -> (i64, i64, String, PathBuf, String) {
    let (repo, bare) = app_repo(&h.dir, Origin::GitHub);
    approved_piece_in(h, rules, &repo, bare).await
}

/// [`approved_piece`] for an app folder the test made.
async fn approved_piece_in(
    h: &Hub,
    rules: Value,
    repo: &Path,
    bare: PathBuf,
) -> (i64, i64, String, PathBuf, String) {
    let main_before = branch_at(&bare, "main").expect("main");
    let app = ok(
        &h.r,
        OWNER,
        "POST",
        "/api/apps",
        json!({"name": "demo", "repo_path": repo.to_string_lossy(), "profile": "A"}),
    )
    .await["id"]
        .as_i64()
        .expect("app id");
    ok(
        &h.r,
        OWNER,
        "POST",
        &format!("/api/apps/{app}/rules"),
        rules,
    )
    .await;
    let goal = ok(
        &h.r,
        OWNER,
        "POST",
        "/api/goals",
        json!({"app_id": app, "text": "Fix the login page"}),
    )
    .await["id"]
        .as_i64()
        .expect("goal");
    run_helper(h, "coordinator", r#"{"say": "Plan.", "plan": {"summary": "Fix it.", "pieces": [{"title": "Fix the login page", "brief": "Fix src/login.rs.", "done_when": "You can sign in."}]}}"#).await;
    let v = ok(
        &h.r,
        OWNER,
        "GET",
        &format!("/api/goals/{goal}"),
        Value::Null,
    )
    .await;
    let plan = v["messages"]
        .as_array()
        .and_then(|m| m.last())
        .expect("plan")["id"]
        .clone();
    // The owner's one tap.
    ok(
        &h.r,
        OWNER,
        "POST",
        &format!("/api/goals/{goal}/approve"),
        json!({"plan_message_id": plan}),
    )
    .await;
    run_helper(h, "design", "Drawn.").await;
    let work = ok(
        &h.r,
        RUNNER,
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
    let head = save_work(h, thread, &branch).await;
    ok(
        &h.r,
        RUNNER,
        "POST",
        &format!("/api/threads/{thread}/events"),
        json!({"kind":"finished","ok":true,"summary":"Fixed the login form."}),
    )
    .await;
    ok(
        &h.r,
        RUNNER,
        "POST",
        &format!("/api/threads/{thread}/move"),
        json!({"to":"in_review"}),
    )
    .await;

    (goal, thread, head, bare, main_before)
}

#[tokio::test]
async fn a_piece_that_passes_its_check_goes_to_github_on_the_plans_approval() {
    let h = hub(Tok::Fixed).await;
    let (goal, thread, head, bare, main_before) = approved_piece(&h, json!({})).await;
    // The checker passes it: no Accept tap, and it goes to GitHub.
    run_helper(
        &h,
        "review",
        r#"{"pass": true, "summary": "Sign-in works."}"#,
    )
    .await;
    let t = settled(&h, thread).await;
    assert_eq!(t["state"], "done");
    assert_eq!(t["github"]["state"], "sent", "{t}");
    assert_eq!(t["github"]["url"], "https://github.com/acme/demo/pull/1");
    let sent_branch = format!("decurion/demo-{thread}-fix-the-login-page");
    assert_eq!(
        branch_at(&bare, &sent_branch).as_deref(),
        Some(head.as_str())
    );
    assert_eq!(
        branch_at(&bare, "main"),
        Some(main_before),
        "never the default branch"
    );
    let opened: Vec<_> = requests(&h).into_iter().filter(|s| s.0 == "POST").collect();
    assert_eq!(opened.len(), 1);
    // The audit says who moved it and why.
    let why: Vec<String> = sqlx::query_scalar(
        "SELECT note FROM audit WHERE thread_id = ? AND actor = 'hub' AND to_state = 'done'",
    )
    .bind(thread)
    .fetch_all(h.store.pool())
    .await
    .expect("audit");
    assert!(
        why.iter().any(|n| n.contains("owner approved the plan")),
        "{why:?}"
    );

    // When the owner merges it on GitHub, the watch sees it and the goal
    // closes by itself.
    h.fake.lock().expect("fake").merged.push(1);
    assert_eq!(decurion_hub::pr_watch::poll(&h.state).await, 1);
    let v = ok(
        &h.r,
        OWNER,
        "GET",
        &format!("/api/goals/{goal}"),
        Value::Null,
    )
    .await;
    assert_eq!(v["goal"]["state"], "done", "{v}");
}

#[tokio::test]
async fn with_the_rule_off_a_passed_piece_waits_for_the_owners_accept() {
    let h = hub(Tok::Fixed).await;
    let (goal, thread, _head, _bare, _main) =
        approved_piece(&h, json!({"send_checked": false})).await;
    run_helper(
        &h,
        "review",
        r#"{"pass": true, "summary": "Sign-in works."}"#,
    )
    .await;
    let t = ok(
        &h.r,
        OWNER,
        "GET",
        &format!("/api/threads/{thread}"),
        Value::Null,
    )
    .await;
    assert_eq!(t["state"], "in_review", "{t}");
    assert!(t["github"].is_null(), "{t}");
    assert!(requests(&h).iter().all(|s| s.0 != "POST"), "nothing opened");
    let v = ok(
        &h.r,
        OWNER,
        "GET",
        &format!("/api/goals/{goal}"),
        Value::Null,
    )
    .await;
    assert!(v.to_string().contains("waits for your Accept"), "{v}");
    // Its work panel knows the app's rule, so it asks for Accept.
    let goal_of = |h: &Hub| {
        let r = h.r.clone();
        async move {
            ok(
                &r,
                OWNER,
                "GET",
                &format!("/api/threads/{thread}/goal"),
                Value::Null,
            )
            .await
        }
    };
    let of = goal_of(&h).await;
    assert_eq!(of["sends_checked"], false, "{of}");
    assert_eq!(of["review"], "passed", "{of}");
    assert_eq!(of["by_plan"], false, "{of}");
    // The owner's Accept sends it, as for any work.
    accept(&h, thread, "accept-after-check").await;
    let t = settled(&h, thread).await;
    assert_eq!(t["github"]["state"], "sent", "{t}");
    // The owner accepted it, not the plan: the panel must not say otherwise.
    let of = goal_of(&h).await;
    assert_eq!(of["by_plan"], false, "{of}");
}

/// A sent pull request whose app may be merged from Decurion, with GitHub
/// showing Decurion's commit at its head.
async fn mergeable_pr(h: &Hub, merge: bool) -> (i64, String) {
    let (_repo, _bare, thread, head, _branch) = sent_pr(h).await;
    ok(
        &h.r,
        OWNER,
        "POST",
        "/api/apps/1/rules",
        json!({"merge": merge, "receipt": format!("rules-{merge}")}),
    )
    .await;
    h.fake.lock().expect("fake").heads.insert(1, head.clone());
    (thread, head)
}

fn merges(h: &Hub) -> Vec<(String, String, String, Value)> {
    requests(h).into_iter().filter(|s| s.0 == "PUT").collect()
}

#[tokio::test]
async fn an_app_at_pr_only_never_merges_from_decurion() {
    let h = hub(Tok::Fixed).await;
    let (thread, _) = mergeable_pr(&h, false).await;
    assert_eq!(watched(&h).await[0]["can_merge"], false);
    let (s, v) = call(
        &h.r,
        OWNER,
        "POST",
        &format!("/api/threads/{thread}/pr/merge"),
        json!({"receipt": "merge-receipt-1"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(
        v.as_str()
            .expect("text")
            .contains("merge its pull requests on GitHub"),
        "{v}"
    );
    assert!(merges(&h).is_empty());
    assert_eq!(watched(&h).await[0]["state"], "open");
}

#[tokio::test]
async fn the_owners_merge_tap_merges_decurions_commit_once() {
    let h = hub(Tok::Fixed).await;
    let (thread, head) = mergeable_pr(&h, true).await;
    let w = watched(&h).await;
    assert_eq!(w[0]["can_merge"], true, "{w}");
    let today = ok(&h.r, OWNER, "GET", "/api/today", Value::Null).await;
    assert_eq!(today[0]["last_pr"]["can_merge"], true, "{today}");

    // Only the owner taps it.
    let path = format!("/api/threads/{thread}/pr/merge");
    let (s, _) = call(
        &h.r,
        RUNNER,
        "POST",
        &path,
        json!({"receipt": "merge-receipt-r"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(merges(&h).is_empty());

    let v = ok(
        &h.r,
        OWNER,
        "POST",
        &path,
        json!({"receipt": "merge-receipt-1"}),
    )
    .await;
    assert_eq!(v["state"], "merged", "{v}");
    assert_eq!(v["event"], "Merged from Decurion");
    assert_eq!(v["can_merge"], false);
    let m = merges(&h);
    assert_eq!(m.len(), 1, "{m:?}");
    assert_eq!(m[0].1, "merge/1");
    assert_eq!(m[0].2, format!("Bearer {TOKEN}"));
    assert_eq!(m[0].3, json!({"sha": head, "merge_method": "squash"}));

    // A resent tap merges nothing twice, and watching is over.
    let again = ok(
        &h.r,
        OWNER,
        "POST",
        &path,
        json!({"receipt": "merge-receipt-1"}),
    )
    .await;
    assert_eq!(again["state"], "merged");
    assert_eq!(merges(&h).len(), 1);
    assert_eq!(decurion_hub::pr_watch::poll(&h.state).await, 0);
}

#[tokio::test]
async fn merge_waits_until_github_says_the_pull_request_is_ready() {
    let h = hub(Tok::Fixed).await;
    let (thread, _) = mergeable_pr(&h, true).await;
    let path = format!("/api/threads/{thread}/pr/merge");
    for (state, says) in [
        ("unstable", "checks"),
        ("blocked", "review"),
        ("dirty", "conflicts"),
        ("unknown", "still checking"),
    ] {
        h.fake
            .lock()
            .expect("fake")
            .merge_state
            .insert(1, state.into());
        let (s, v) = call(
            &h.r,
            OWNER,
            "POST",
            &path,
            json!({"receipt": format!("merge-{state}")}),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT, "{state}: {v}");
        assert!(v.as_str().expect("text").contains(says), "{state}: {v}");
    }
    // Someone pushed to it after Decurion did.
    {
        let mut f = h.fake.lock().expect("fake");
        f.merge_state.insert(1, "clean".into());
        f.heads.insert(1, "f".repeat(40));
    }
    let (s, v) = call(
        &h.r,
        OWNER,
        "POST",
        &path,
        json!({"receipt": "merge-moved"}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(v.as_str().expect("text").contains("pushed"), "{v}");
    assert!(merges(&h).is_empty(), "never asked GitHub to merge");
    assert_eq!(watched(&h).await[0]["state"], "open");
}

// ---------- One story from request to live ----------

async fn goal_view(h: &Hub, goal: i64) -> Value {
    ok(
        &h.r,
        OWNER,
        "GET",
        &format!("/api/goals/{goal}"),
        Value::Null,
    )
    .await
}

fn road_now(road: &Value) -> String {
    road["steps"]
        .as_array()
        .and_then(|s| s.iter().find(|s| s["mark"] == "now"))
        .and_then(|s| s["label"].as_str())
        .unwrap_or("")
        .to_owned()
}

#[tokio::test]
async fn a_pull_request_opened_from_decurions_link_is_found_and_watched_until_the_goal_closes() {
    // No token when the piece goes up, so Decurion gives a link instead of
    // opening the pull request.
    let h = hub(Tok::None).await;
    let (goal, thread, _head, _bare, _main) = approved_piece(&h, json!({})).await;
    let v = goal_view(&h, goal).await;
    assert!(
        v["story"]
            .as_str()
            .expect("story")
            .contains("goes to GitHub by itself"),
        "{v}"
    );
    run_helper(
        &h,
        "review",
        r#"{"pass": true, "summary": "Sign-in works."}"#,
    )
    .await;
    let t = settled(&h, thread).await;
    assert_eq!(t["github"]["pull_request"], false, "{t}");
    let road = &goal_view(&h, goal).await["road"];
    assert_eq!(road_now(road), "Merged", "{road}");
    assert_eq!(road["label"], "Needs you · open the pull request", "{road}");
    let labels: Vec<_> = road["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .map(|s| s["label"].as_str().unwrap_or("").to_owned())
        .collect();
    assert_eq!(labels, ["Built", "Checked", "On GitHub", "Merged"]);
    let of = ok(
        &h.r,
        OWNER,
        "GET",
        &format!("/api/threads/{thread}/goal"),
        Value::Null,
    )
    .await;
    assert_eq!(of["road"], *road, "the panel tells the same story");

    // With a token, Decurion looks for it. Someone's pull request from
    // another branch is never taken up.
    let state = h
        .state
        .clone()
        .with_github(GitHub::new(&h.api, Tokens::Fixed(TOKEN.into())).expect("github"));
    h.fake.lock().expect("fake").prs.push((
        "someone/else".into(),
        "https://github.com/acme/demo/pull/1".into(),
    ));
    assert_eq!(decurion_hub::pr_watch::poll(&state).await, 0);
    assert!(watched(&h).await.as_array().expect("list").is_empty());

    // The owner opens it from Decurion's link: its head is Decurion's branch.
    let branch = t["github"]["branch"].as_str().expect("branch").to_owned();
    h.fake
        .lock()
        .expect("fake")
        .prs
        .push((branch, "https://github.com/acme/demo/pull/2".into()));
    assert_eq!(decurion_hub::pr_watch::poll(&state).await, 1);
    let w = watched(&h).await;
    assert_eq!(w[0]["thread_id"], thread, "{w}");
    assert_eq!(w[0]["number"], 2);
    let t = ok(
        &h.r,
        OWNER,
        "GET",
        &format!("/api/threads/{thread}"),
        Value::Null,
    )
    .await;
    assert_eq!(t["github"]["pull_request"], true, "{t}");
    assert_eq!(t["github"]["url"], "https://github.com/acme/demo/pull/2");
    let road = &goal_view(&h, goal).await["road"];
    assert_eq!(road["label"], "Needs you · merge on GitHub", "{road}");

    // Merged on GitHub: the app does not deploy, so merged is the end and
    // the goal closes by itself.
    h.fake.lock().expect("fake").merged.push(2);
    assert_eq!(decurion_hub::pr_watch::poll(&state).await, 1);
    let v = goal_view(&h, goal).await;
    assert_eq!(v["goal"]["state"], "done", "{v}");
    assert_eq!(v["road"]["done"], true, "{v}");
    assert_eq!(v["road"]["label"], "Merged", "{v}");
    // Looking again finds nothing more to take up.
    let before = requests(&h).len();
    assert_eq!(decurion_hub::pr_watch::poll(&state).await, 0);
    assert_eq!(requests(&h).len(), before);
}

#[tokio::test]
async fn decurions_own_change_waits_for_an_update_then_is_live_and_the_goal_closes() {
    let h = hub(Tok::Fixed).await;
    // The app is Decurion's own code: its clone has Decurion's files.
    let (repo, bare) = app_repo(&h.dir, Origin::GitHub);
    write(&repo, "crates/decurion-hub/build.rs", "fn main() {}\n");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "decurion"]);
    git(&repo, &["push", "--quiet", &bare.to_string_lossy(), "main"]);
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let running = git(&repo, &["rev-parse", "HEAD"]);
    let (goal, thread, _head, _bare, _main) = approved_piece_in(&h, json!({}), &repo, bare).await;
    let v = goal_view(&h, goal).await;
    let story = v["story"].as_str().expect("story");
    assert!(
        story.contains("keeps running its old version until you update it"),
        "{story}"
    );
    run_helper(
        &h,
        "review",
        r#"{"pass": true, "summary": "Sign-in works."}"#,
    )
    .await;
    settled(&h, thread).await;
    let road = &goal_view(&h, goal).await["road"];
    let last = road["steps"]
        .as_array()
        .and_then(|s| s.last())
        .expect("steps");
    assert_eq!(last["label"], "Update Decurion", "{road}");

    // GitHub merges it; the owner's clone gets the merge when they pull.
    git(
        &repo,
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Merge pull request #1",
        ],
    );
    let merge = git(&repo, &["rev-parse", "HEAD"]);
    {
        let mut f = h.fake.lock().expect("fake");
        f.merge_commits.insert(1, merge.clone());
        f.merged.push(1);
    }
    assert_eq!(decurion_hub::pr_watch::poll(&h.state).await, 1);
    let v = goal_view(&h, goal).await;
    assert_ne!(v["goal"]["state"], "done", "merged is not live here: {v}");
    let road = &v["road"];
    assert_eq!(road_now(road), "Update Decurion", "{road}");
    assert_eq!(road["you"], true);
    assert!(
        road["next"].as_str().expect("next").starts_with(
            "Merged. This Decurion still runs the old version. Update it to use the change."
        ),
        "{road}"
    );
    assert!(
        !road["update"].as_array().expect("how").is_empty(),
        "{road}"
    );

    // The Decurion still running the old commit is not live.
    assert!(h
        .store
        .mark_self_live(&running)
        .await
        .expect("look")
        .is_empty());
    assert_eq!(
        goal_view(&h, goal).await["goal"]["state"],
        v["goal"]["state"]
    );
    // A hub that does not know its commit marks nothing.
    assert!(h
        .store
        .mark_self_live("unknown")
        .await
        .expect("look")
        .is_empty());

    // The owner updates: the hub now runs a commit that holds the merge.
    git(
        &repo,
        &["commit", "--quiet", "--allow-empty", "-m", "later"],
    );
    let updated = git(&repo, &["rev-parse", "HEAD"]);
    let before = git(&repo, &["status", "--porcelain"]);
    let news = h.store.mark_self_live(&updated).await.expect("look");
    assert_eq!(news.len(), 1, "the goal closed");
    assert_eq!(
        git(&repo, &["rev-parse", "HEAD"]),
        updated,
        "the clone is only read"
    );
    assert_eq!(git(&repo, &["status", "--porcelain"]), before);
    let v = goal_view(&h, goal).await;
    assert_eq!(v["goal"]["state"], "done", "{v}");
    assert_eq!(v["road"]["done"], true);
    assert_eq!(v["road"]["label"], "Live");
    // Once is enough.
    assert!(h
        .store
        .mark_self_live(&updated)
        .await
        .expect("again")
        .is_empty());
}

// ---------- An honest GitHub light ----------

fn github_light(v: &Value) -> Value {
    v["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["key"] == "github")
        .cloned()
        .expect("github row")
}

#[tokio::test]
async fn a_token_that_cannot_open_pull_requests_turns_github_amber_until_one_opens() {
    let h = hub(Tok::Fixed).await;
    let (repo, _bare) = app_repo(&h.dir, Origin::GitHub);
    h.fake.lock().expect("fake").refuse_open = Some(403);
    let (thread, _head) = reviewed_work(&h, &repo).await;
    accept(&h, thread, "accept-receipt-no-pr").await;
    let t = settled(&h, thread).await;
    let g = &t["github"];
    // The work still went up as a branch, with the link and the plain reason.
    assert_eq!(g["state"], "sent", "{g}");
    assert_eq!(g["pull_request"], false);
    assert!(g["url"].as_str().expect("url").contains("/compare/"), "{g}");
    let why = g["error"].as_str().expect("reason");
    assert!(
        why.contains("can't open pull requests on this app"),
        "{why}"
    );
    assert!(why.contains("Settings, Sign-ins"), "{why}");

    // Settings, Sign-ins says so, with steps and GitHub's token page.
    let v = ok(&h.r, OWNER, "GET", "/api/sign-in-lights", Value::Null).await;
    let row = github_light(&v);
    assert_eq!(row["light"], "amber", "{row}");
    assert_eq!(
        row["text"],
        "This PC's GitHub sign-in can send work but can't open pull requests on demo."
    );
    assert!(
        row["steps"].as_array().is_some_and(|s| s.len() >= 3),
        "{row}"
    );
    assert_eq!(
        row["link"]["url"],
        "https://github.com/settings/personal-access-tokens"
    );
    assert_eq!(v["amber"], 1, "{v}");
    // The token is never in what the page gets, nor in what was kept.
    assert!(!v.to_string().contains(TOKEN));
    let kept: Vec<String> = sqlx::query_scalar("SELECT reason || at FROM github_pr_trouble")
        .fetch_all(h.store.pool())
        .await
        .expect("trouble");
    assert_eq!(kept.len(), 1);
    assert!(kept.iter().all(|k| !k.contains(TOKEN)));

    // Once the token may open them, the next piece's pull request opens and
    // the light is green again.
    h.fake.lock().expect("fake").refuse_open = None;
    let app: i64 = sqlx::query_scalar("SELECT app_id FROM threads WHERE id = ?")
        .bind(thread)
        .fetch_one(h.store.pool())
        .await
        .expect("app");
    let (next, _) = reviewed_work_in(&h, app).await;
    accept(&h, next, "accept-receipt-pr-again").await;
    let t = settled(&h, next).await;
    assert_eq!(t["github"]["pull_request"], true, "{t}");
    let v = ok(&h.r, OWNER, "GET", "/api/sign-in-lights", Value::Null).await;
    assert_eq!(github_light(&v)["light"], "green", "{v}");
    assert!(github_light(&v)["steps"].is_null(), "{v}");
}

#[tokio::test]
async fn a_token_saved_after_the_trouble_is_given_its_chance() {
    let h = hub(Tok::Settings).await;
    ok(
        &h.r,
        OWNER,
        "POST",
        "/api/github/token",
        json!({"value": TOKEN}),
    )
    .await;
    let (repo, _bare) = app_repo(&h.dir, Origin::GitHub);
    h.fake.lock().expect("fake").refuse_open = Some(404);
    let (thread, _head) = reviewed_work(&h, &repo).await;
    accept(&h, thread, "accept-receipt-saved").await;
    settled(&h, thread).await;
    let v = ok(&h.r, OWNER, "GET", "/api/sign-in-lights", Value::Null).await;
    let row = github_light(&v);
    assert_eq!(row["light"], "amber", "{row}");
    assert_eq!(
        row["text"],
        "Your GitHub token can send work but can't open pull requests on demo."
    );
    // The owner saves a new token: it may well work, so the light stops
    // saying it can't until a send says so again.
    sqlx::query(
        "UPDATE github_pr_trouble SET at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 minute')",
    )
    .execute(h.store.pool())
    .await
    .expect("age");
    ok(
        &h.r,
        OWNER,
        "POST",
        "/api/github/token",
        json!({"value": format!("{TOKEN}x")}),
    )
    .await;
    let v = ok(&h.r, OWNER, "GET", "/api/sign-in-lights", Value::Null).await;
    assert_ne!(github_light(&v)["light"], "amber", "{v}");
}

#[tokio::test]
async fn a_pull_request_that_clashes_with_main_says_so() {
    let h = hub(Tok::Fixed).await;
    let (_repo, _bare, thread, _head, _branch) = sent_pr(&h).await;
    let t = ok(
        &h.r,
        OWNER,
        "GET",
        &format!("/api/threads/{thread}"),
        Value::Null,
    )
    .await;
    assert_eq!(t["github"]["clash"], false, "{t}");

    h.fake
        .lock()
        .expect("fake")
        .merge_state
        .insert(1, "dirty".into());
    decurion_hub::pr_watch::poll(&h.state).await;
    let t = ok(
        &h.r,
        OWNER,
        "GET",
        &format!("/api/threads/{thread}"),
        Value::Null,
    )
    .await;
    assert_eq!(t["github"]["clash"], true, "{t}");
    assert_eq!(watched(&h).await[0]["clash"], true);

    // GitHub still working it out tells nothing; fixed, it is clear again.
    h.fake
        .lock()
        .expect("fake")
        .merge_state
        .insert(1, "unknown".into());
    decurion_hub::pr_watch::poll(&h.state).await;
    assert_eq!(watched(&h).await[0]["clash"], true);
    h.fake
        .lock()
        .expect("fake")
        .merge_state
        .insert(1, "clean".into());
    decurion_hub::pr_watch::poll(&h.state).await;
    assert_eq!(watched(&h).await[0]["clash"], false);
    let t = ok(
        &h.r,
        OWNER,
        "GET",
        &format!("/api/threads/{thread}"),
        Value::Null,
    )
    .await;
    assert_eq!(t["github"]["clash"], false, "{t}");
}

// ---------- Bring up to date ----------

/// Lets the app's folder fetch from its GitHub address too (the local bare
/// repo stands in for GitHub), as Bring up to date does.
fn fetch_from(repo: &Path, bare: &Path) {
    let bare_s = bare.to_string_lossy().into_owned();
    git(
        repo,
        &["config", &format!("url.{bare_s}.insteadOf"), GITHUB_URL],
    );
}

/// Points the app's folder's GitHub address at a server that is not there.
fn server_gone(h: &Hub, repo: &Path, bare: &Path) {
    let gone = h.dir.join("gone.git").to_string_lossy().into_owned();
    git(
        repo,
        &[
            "config",
            "--unset",
            &format!("url.{}.insteadOf", bare.to_string_lossy()),
        ],
    );
    git(
        repo,
        &["config", &format!("url.{gone}.insteadOf"), GITHUB_URL],
    );
}

/// Someone else moves the remote's main on, with `file` changed. The
/// owner's folder does not have it. Returns the new commit.
fn main_moves(h: &Hub, bare: &Path, file: &str, text: &str) -> String {
    let other = h.dir.join(format!("other-{}", uuid::Uuid::new_v4()));
    git(
        &h.dir,
        &[
            "clone",
            "--quiet",
            "-b",
            "main",
            &bare.to_string_lossy(),
            &other.to_string_lossy(),
        ],
    );
    write(&other, file, text);
    git(&other, &["add", "-A"]);
    git(&other, &["commit", "--quiet", "-m", "main moves on"]);
    git(&other, &["push", "--quiet", "origin", "main"]);
    git(&other, &["rev-parse", "HEAD"])
}

/// GitHub says the pull request is in `state`, about commit `head`.
fn github_says(h: &Hub, state: &str, head: &str) {
    let mut f = h.fake.lock().expect("fake");
    f.merge_state.insert(1, state.into());
    f.heads.insert(1, head.into());
}

async fn bring_up(h: &Hub, thread: i64, receipt: &str) -> (StatusCode, Value) {
    call(
        &h.r,
        OWNER,
        "POST",
        &format!("/api/threads/{thread}/pr/update"),
        json!({ "receipt": receipt }),
    )
    .await
}

async fn thread_count(h: &Hub) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM threads")
        .fetch_one(h.store.pool())
        .await
        .expect("count")
}

fn refused(s: StatusCode, v: &Value, words: &str) {
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(v.as_str().expect("text").contains(words), "{words}: {v}");
}

#[tokio::test]
async fn bring_up_to_date_starts_one_run_on_the_pull_request_with_main_pinned() {
    let h = hub(Tok::Fixed).await;
    let (repo, bare, thread, head, _branch) = sent_pr(&h).await;
    fetch_from(&repo, &bare);
    // Not read from GitHub yet: Decurion does not know the base branch.
    let (s, v) = bring_up(&h, thread, "update-receipt-0").await;
    refused(s, &v, "not read this pull request");
    github_says(&h, "behind", &head);
    decurion_hub::pr_watch::poll(&h.state).await;
    let main = main_moves(&h, &bare, "README.md", "hello again\n");
    assert_ne!(git(&repo, &["rev-parse", "HEAD"]), main);

    let path = format!("/api/threads/{thread}/pr/update");
    let (s, _) = call(
        &h.r,
        RUNNER,
        "POST",
        &path,
        json!({"receipt": "update-receipt-1"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, run) = bring_up(&h, thread, "update-receipt-1").await;
    assert!(s.is_success(), "{run}");
    assert_eq!(run["fix_of"], thread);
    assert_eq!(run["state"], "queued");
    assert_eq!(run["place"], "pc");
    assert_eq!(run["title"], "Bring up to date: Fix the login page");
    let prompt = run["prompt"].as_str().expect("prompt");
    assert!(
        prompt.contains("git merge --no-ff --no-edit decurion-main"),
        "{prompt}"
    );
    assert!(prompt.contains("Never rebase"), "{prompt}");
    assert!(prompt.contains("Do not push"), "{prompt}");
    let id = run["id"].as_i64().expect("id");
    let (base, from): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT base_commit, merge_from FROM threads WHERE id = ?")
            .bind(id)
            .fetch_one(h.store.pool())
            .await
            .expect("row");
    // It starts from the commit in the pull request and merges the
    // server's newest main, which the hub pinned.
    assert_eq!(base.as_deref(), Some(head.as_str()));
    assert_eq!(from.as_deref(), Some(main.as_str()));
    let app: i64 = run["app_id"].as_i64().expect("app");
    let cache = h
        .dir
        .join("saved")
        .join("repos")
        .join(format!("app-{app}.git"));
    assert_eq!(
        git(
            &cache,
            &["rev-parse", &format!("refs/decurion/start/{main}")]
        ),
        main
    );

    // The same tap again starts nothing new, even with the server gone.
    server_gone(&h, &repo, &bare);
    let (s, again) = bring_up(&h, thread, "update-receipt-1").await;
    assert!(s.is_success(), "{again}");
    assert_eq!(again["id"], id);
    let (s, v) = bring_up(&h, thread, "update-receipt-2").await;
    refused(s, &v, "already under way");
    assert_eq!(thread_count(&h).await, 2);
    let w = watched(&h).await;
    assert_eq!(w[0]["updating"], true, "{w}");
    assert_eq!(w[0]["fixing"], true, "{w}");
    // It stays on the PC.
    let (s, v) = call(
        &h.r,
        OWNER,
        "POST",
        &format!("/api/threads/{id}/place"),
        json!({"place": "cloud"}),
    )
    .await;
    refused(s, &v, "this PC only");
}

#[tokio::test]
async fn bring_up_to_date_is_refused_with_nothing_started_when_it_cannot_or_need_not() {
    let h = hub(Tok::Fixed).await;
    let (repo, bare, thread, head, _branch) = sent_pr(&h).await;
    fetch_from(&repo, &bare);
    github_says(&h, "clean", &head);
    decurion_hub::pr_watch::poll(&h.state).await;
    // Main has not moved: the pull request already has it.
    let (s, v) = bring_up(&h, thread, "update-refused-1").await;
    refused(s, &v, "Already up to date with main");
    // The server cannot be reached: nothing is fetched or started.
    main_moves(&h, &bare, "README.md", "newer\n");
    server_gone(&h, &repo, &bare);
    let (s, v) = bring_up(&h, thread, "update-refused-2").await;
    refused(s, &v, "could not get the newest main");
    assert_eq!(thread_count(&h).await, 1);
    // A pull request that ended has nothing to bring up to date.
    h.fake.lock().expect("fake").merged.push(1);
    decurion_hub::pr_watch::poll(&h.state).await;
    let (s, v) = bring_up(&h, thread, "update-refused-3").await;
    refused(s, &v, "merged or closed");
    assert_eq!(thread_count(&h).await, 1);
}

/// GitHub says it conflicts, though the pull request already holds main's
/// newest commit: the owner can still start it.
#[tokio::test]
async fn a_conflicting_pull_request_can_be_brought_up_to_date_whatever_main_is_at() {
    let h = hub(Tok::Fixed).await;
    let (repo, bare, thread, head, _branch) = sent_pr(&h).await;
    fetch_from(&repo, &bare);
    github_says(&h, "dirty", &head);
    decurion_hub::pr_watch::poll(&h.state).await;
    let (s, v) = bring_up(&h, thread, "update-dirty-1").await;
    assert!(s.is_success(), "{v}");
    assert_eq!(watched(&h).await[0]["health"], "Conflicts with main");
}

/// Like [`main_moves`], with several files.
fn main_moves_with(h: &Hub, bare: &Path, files: &[(&str, &str)]) -> String {
    let other = h.dir.join(format!("other-{}", uuid::Uuid::new_v4()));
    git(
        &h.dir,
        &[
            "clone",
            "--quiet",
            "-b",
            "main",
            &bare.to_string_lossy(),
            &other.to_string_lossy(),
        ],
    );
    for (file, text) in files {
        write(&other, file, text);
    }
    git(&other, &["add", "-A"]);
    git(&other, &["commit", "--quiet", "-m", "main moves on"]);
    git(&other, &["push", "--quiet", "origin", "main"]);
    git(&other, &["rev-parse", "HEAD"])
}

/// How the test's agent finishes a Bring up to date run.
#[derive(Clone, Copy)]
enum Agent {
    /// `git merge --no-ff --no-edit decurion-main`, as asked.
    Merges,
    /// Merges, and changes the protected src/app.rs in the merge.
    EditsProtected,
    /// Merges, but keeps the pull request's old copy of src/app.rs, which
    /// main had changed.
    KeepsOldCopy,
    /// Never merges: one ordinary commit.
    DoesNotMerge,
}

/// Plays the runner for a queued Bring up to date run: claims it, builds
/// its copy from the starting bundle (both refs), lets the agent work,
/// saves it and puts it in review. Returns the work's commit.
async fn run_update(h: &Hub, id: i64, main: &str, agent: Agent) -> String {
    let work = ok(
        &h.r,
        RUNNER,
        "POST",
        "/api/runner/claim",
        json!({"runner":"local","rules_version":decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(work["thread"]["id"], id);
    assert_eq!(work["merge_from"], main);
    let branch = work["thread"]["branch"]
        .as_str()
        .expect("branch")
        .to_owned();
    let (s, bundle, headers) = send(
        &h.r,
        RUNNER,
        "GET",
        &format!("/api/runner/threads/{id}/base"),
        Body::empty(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let base = headers["x-decurion-base"].to_str().expect("sha").to_owned();
    let xfer = h.dir.join(format!("xfer-{id}"));
    std::fs::create_dir_all(&xfer).expect("xfer");
    let base_bundle = xfer.join("base.bundle");
    std::fs::write(&base_bundle, bundle).expect("write");
    let wt = h.dir.join(format!("work-{id}"));
    std::fs::create_dir_all(&wt).expect("work dir");
    git(&wt, &["init", "--quiet"]);
    git(
        &wt,
        &[
            "fetch",
            "--quiet",
            &base_bundle.to_string_lossy(),
            &format!("refs/heads/decurion-base:refs/heads/{branch}"),
            "refs/heads/decurion-main:refs/heads/decurion-main",
        ],
    );
    assert_eq!(git(&wt, &["rev-parse", "decurion-main"]), main);
    git(&wt, &["checkout", "--quiet", &branch]);
    match agent {
        Agent::Merges => {
            git(&wt, &["merge", "--no-ff", "--no-edit", "decurion-main"]);
        }
        Agent::EditsProtected | Agent::KeepsOldCopy => {
            git(&wt, &["merge", "--no-ff", "--no-commit", "decurion-main"]);
            if matches!(agent, Agent::EditsProtected) {
                write(&wt, "src/app.rs", "fn main() { pay_more(); }\n");
            } else {
                git(&wt, &["checkout", "HEAD", "--", "src/app.rs"]);
            }
            git(&wt, &["add", "-A"]);
            git(&wt, &["commit", "--quiet", "--no-edit"]);
        }
        Agent::DoesNotMerge => {
            write(&wt, "src/other.rs", "fn other() {}\n");
            git(&wt, &["add", "-A"]);
            git(&wt, &["commit", "--quiet", "-m", "no merge"]);
        }
    }
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
    for (part, data) in [
        ("code", std::fs::read(&code).expect("read")),
        ("session", b"SESSION TAR".to_vec()),
    ] {
        let (s, _, _) = send(
            &h.r,
            RUNNER,
            "PUT",
            &format!("/api/runner/threads/{id}/upload/{part}"),
            Body::from(data),
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT);
    }
    ok(
        &h.r,
        RUNNER,
        "POST",
        &format!("/api/runner/threads/{id}/checkpoint"),
        json!({"head": head, "base": base}),
    )
    .await;
    ok(
        &h.r,
        RUNNER,
        "POST",
        &format!("/api/threads/{id}/events"),
        json!({"kind":"finished","ok":true,"summary":"Merged main."}),
    )
    .await;
    ok(
        &h.r,
        RUNNER,
        "POST",
        &format!("/api/threads/{id}/move"),
        json!({"to":"in_review"}),
    )
    .await;
    head
}

/// A pull request whose main moved on (with src/app.rs now protected and
/// changed there), and a Bring up to date run for it. Returns the folder,
/// its remote, the work that opened the pull request, its branch, main's
/// new commit and the run.
async fn update_started(h: &Hub) -> (PathBuf, PathBuf, i64, String, String, i64) {
    let (repo, bare, thread, head, branch) = sent_pr(h).await;
    fetch_from(&repo, &bare);
    github_says(h, "behind", &head);
    decurion_hub::pr_watch::poll(&h.state).await;
    let main = main_moves_with(
        h,
        &bare,
        &[
            (".decurion/protected-paths", "src/app.rs\n"),
            ("src/app.rs", "fn main() { pay(); }\n"),
        ],
    );
    let (s, run) = bring_up(h, thread, "update-run-receipt").await;
    assert!(s.is_success(), "{run}");
    let id = run["id"].as_i64().expect("id");
    // Fetching is done. `git remote get-url` follows insteadOf, and the
    // send must still see the folder's GitHub address.
    git(
        &repo,
        &[
            "config",
            "--unset",
            &format!("url.{}.insteadOf", bare.to_string_lossy()),
        ],
    );
    (repo, bare, thread, branch, main, id)
}

async fn thread_json(h: &Hub, id: i64) -> Value {
    ok(
        &h.r,
        OWNER,
        "GET",
        &format!("/api/threads/{id}"),
        Value::Null,
    )
    .await
}

#[tokio::test]
async fn an_accepted_update_moves_the_pull_request_forward_with_main_merged() {
    let h = hub(Tok::Fixed).await;
    let (_repo, bare, thread, branch, main, id) = update_started(&h).await;
    let pr_head = branch_at(&bare, &branch).expect("pr");
    let head = run_update(&h, id, &main, Agent::Merges).await;
    // Main changed a protected file, but the pull request does not change
    // it compared with main: nothing is held.
    let t = thread_json(&h, id).await;
    assert_eq!(t["state"], "in_review", "{t}");
    assert_eq!(t["hold"], Value::Null, "{t}");
    accept(&h, id, "accept-receipt-update").await;
    let t = settled(&h, id).await;
    assert_eq!(t["github"]["state"], "sent", "{t}");
    assert_eq!(t["github"]["url"], "https://github.com/acme/demo/pull/1");
    // A plain fast-forward on the same branch, a merge of the old head and
    // main; main itself untouched, and no new pull request.
    assert_eq!(branch_at(&bare, &branch).as_deref(), Some(head.as_str()));
    assert_eq!(updates(&bare, &branch), 2);
    assert_eq!(git(&bare, &["rev-parse", &format!("{head}^1")]), pr_head);
    assert_eq!(git(&bare, &["rev-parse", &format!("{head}^2")]), main);
    assert_eq!(branch_at(&bare, "main").as_deref(), Some(main.as_str()));
    assert_eq!(
        requests(&h)
            .iter()
            .filter(|s| s.1 == "pulls" && s.0 == "POST")
            .count(),
        1
    );
    let w = watched(&h).await;
    assert_eq!(w[0]["event"], "Brought up to date", "{w}");
    assert_eq!(w[0]["health"], Value::Null, "{w}");
    assert_eq!(w[0]["updating"], false, "{w}");
    let _ = thread;
}

#[tokio::test]
async fn an_update_that_changes_a_protected_part_compared_with_main_is_held() {
    for agent in [Agent::EditsProtected, Agent::KeepsOldCopy] {
        let h = hub(Tok::Fixed).await;
        let (_repo, _bare, _thread, _branch, main, id) = update_started(&h).await;
        run_update(&h, id, &main, agent).await;
        let t = thread_json(&h, id).await;
        assert_eq!(t["state"], "in_review", "{t}");
        let why = t["hold"].as_str().expect("held");
        assert!(why.starts_with("Held for you:"), "{why}");
        assert!(why.contains("src/app.rs"), "{why}");
    }
}

#[tokio::test]
async fn an_update_that_did_not_merge_main_or_whose_pull_request_moved_is_never_sent() {
    // Not a merge of main: the hub checks again and refuses.
    let h = hub(Tok::Fixed).await;
    let (_repo, bare, _thread, branch, main, id) = update_started(&h).await;
    let before = branch_at(&bare, &branch);
    run_update(&h, id, &main, Agent::DoesNotMerge).await;
    accept(&h, id, "accept-receipt-nomerge").await;
    let t = settled(&h, id).await;
    assert_eq!(t["github"]["state"], "failed", "{t}");
    let why = t["github"]["error"].as_str().expect("why");
    assert!(why.contains("did not merge main"), "{why}");
    assert_eq!(branch_at(&bare, &branch), before);

    // The pull request now merges into another branch.
    let h = hub(Tok::Fixed).await;
    let (_repo, bare, _thread, branch, main, id) = update_started(&h).await;
    let before = branch_at(&bare, &branch);
    run_update(&h, id, &main, Agent::Merges).await;
    h.fake
        .lock()
        .expect("fake")
        .bases
        .insert(1, "develop".into());
    accept(&h, id, "accept-receipt-moved").await;
    let t = settled(&h, id).await;
    assert_eq!(t["github"]["state"], "failed", "{t}");
    let why = t["github"]["error"].as_str().expect("why");
    assert!(why.contains("different branch"), "{why}");
    assert_eq!(branch_at(&bare, &branch), before);

    // Something else reached the pull request first: this update is stale
    // and is refused, never forced over it.
    let h = hub(Tok::Fixed).await;
    let (_repo, bare, _thread, branch, main, id) = update_started(&h).await;
    run_update(&h, id, &main, Agent::Merges).await;
    let cache = h.dir.join("saved").join("repos").join("app-1.git");
    let pr_head = branch_at(&bare, &branch).expect("pr");
    let other = git(
        &cache,
        &[
            "commit-tree",
            &format!("{pr_head}^{{tree}}"),
            "-p",
            &pr_head,
            "-m",
            "another update, sent first",
        ],
    );
    sqlx::query("UPDATE pr_watches SET head = ?")
        .bind(&other)
        .execute(h.store.pool())
        .await
        .expect("moved");
    let before = branch_at(&bare, &branch);
    accept(&h, id, "accept-receipt-stale").await;
    let t = settled(&h, id).await;
    assert_eq!(t["github"]["state"], "failed", "{t}");
    let why = t["github"]["error"].as_str().expect("why");
    assert!(why.contains("does not build on"), "{why}");
    assert_eq!(branch_at(&bare, &branch), before);
    assert_eq!(updates(&bare, &branch), 1);
}
