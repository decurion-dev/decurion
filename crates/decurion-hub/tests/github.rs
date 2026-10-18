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
        .map(|(_, u)| json!({ "html_url": u }))
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
    let url = format!("https://github.com/acme/demo/pull/{}", f.prs.len() + 1);
    f.prs.push((
        b["head"].as_str().unwrap_or_default().to_owned(),
        url.clone(),
    ));
    (StatusCode::CREATED, Json(json!({ "html_url": url })))
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
    write(&wt, "src/login.rs", "fn login() {}\n");
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
