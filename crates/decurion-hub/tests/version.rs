//! Which Decurion this PC runs (`GET /api/decurion/version`, and the Decurion
//! app's row on Today). Real git repos stand in for Decurion's own code: a
//! bare "server" and the owner's folder cloned from it.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::{router, store::Store, AppState, Keys};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";

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

/// Decurion's own code: a server ("origin", default branch `trunk`) fed
/// from `seed`, and the owner's folder cloned from it at the first commit.
struct Code {
    dir: PathBuf,
    seed: PathBuf,
    origin: PathBuf,
    folder: PathBuf,
    /// The first commit, which the folder has.
    first: String,
}

impl Code {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("decurion-version-{}", uuid::Uuid::new_v4()));
        let seed = dir.join("seed");
        std::fs::create_dir_all(seed.join("crates/decurion-hub")).expect("seed dir");
        git(&seed, &["init", "--quiet"]);
        // Only Decurion's own code has this file: it makes the app Decurion.
        std::fs::write(seed.join("crates/decurion-hub/build.rs"), "fn main() {}\n").expect("write");
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "--quiet", "-m", "first"]);
        let first = git(&seed, &["rev-parse", "HEAD"]);
        let origin = dir.join("origin.git");
        git(
            &dir,
            &[
                "clone",
                "--quiet",
                "--bare",
                &seed.to_string_lossy(),
                &origin.to_string_lossy(),
            ],
        );
        let folder = dir.join("decurion");
        git(
            &dir,
            &[
                "clone",
                "--quiet",
                &origin.to_string_lossy(),
                &folder.to_string_lossy(),
            ],
        );
        Self {
            dir,
            seed,
            origin,
            folder,
            first,
        }
    }

    /// A change merged on the server, with `subject` as its message. The
    /// owner's folder does not get it.
    fn merge(&self, subject: &str) -> String {
        std::fs::write(self.seed.join("README.md"), subject).expect("write");
        git(&self.seed, &["add", "."]);
        git(&self.seed, &["commit", "--quiet", "-m", subject]);
        git(
            &self.origin,
            &[
                "fetch",
                "--quiet",
                &self.seed.to_string_lossy(),
                "+trunk:trunk",
            ],
        );
        git(&self.seed, &["rev-parse", "HEAD"])
    }

    /// Everything the owner's folder holds that a write could change.
    fn folder_state(&self) -> String {
        let refs = git(&self.folder, &["for-each-ref"]);
        let head = git(&self.folder, &["rev-parse", "HEAD"]);
        let fetch_head = self.folder.join(".git").join("FETCH_HEAD").exists();
        let objects = git(&self.folder, &["count-objects", "-v"]);
        format!("{refs}\n{head}\n{fetch_head}\n{objects}")
    }
}

impl Drop for Code {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Hub {
    r: axum::Router,
    state: AppState,
}

async fn hub(code: &Code, commit: &str) -> Hub {
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let state = AppState::new(
        store,
        Keys {
            owner: OWNER.into(),
            runner: RUNNER.into(),
        },
        "abc123".into(),
        None,
    )
    .with_files(code.dir.join("saved"))
    .with_hub_commit(commit);
    Hub {
        r: router(state.clone()),
        state,
    }
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
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .header(header::CONTENT_TYPE, "application/json")
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

async fn add_app(h: &Hub, code: &Code) -> i64 {
    let (s, app) = call(
        &h.r,
        "POST",
        "/api/apps",
        OWNER,
        json!({"name": "decurion", "repo_path": code.folder.to_string_lossy(), "profile": "A"}),
    )
    .await;
    assert!(s.is_success(), "{s} {app}");
    app["id"].as_i64().expect("app id")
}

/// The version answer once the first look has finished (a slow test
/// machine may take longer than the route waits).
async fn version(h: &Hub) -> Value {
    let until = Instant::now() + Duration::from_secs(120);
    loop {
        let (s, v) = call(&h.r, "GET", "/api/decurion/version", OWNER, Value::Null).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let checking = v["words"]
            .as_str()
            .is_some_and(|w| w.starts_with("Checking"));
        if !checking || Instant::now() > until {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn today_row(h: &Hub, app: i64) -> Value {
    let (s, rows) = call(&h.r, "GET", "/api/today", OWNER, Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{rows}");
    rows.as_array()
        .and_then(|rows| rows.iter().find(|r| r["app_id"] == app))
        .cloned()
        .expect("the app's row")
}

#[tokio::test]
async fn the_owner_sees_which_changes_this_pc_is_not_running_and_the_folder_is_only_read() {
    let code = Code::new();
    code.merge("feat(hub): first thing (#51)");
    code.merge("fix: second thing (#52)");
    let h = hub(&code, &code.first).await;
    let app = add_app(&h, &code).await;
    let before = code.folder_state();

    let v = version(&h).await;
    assert_eq!(v["state"], "behind", "{v}");
    assert_eq!(v["behind"], 2, "{v}");
    assert_eq!(
        v["changes"],
        json!([
            {"title": "Second thing", "number": 52},
            {"title": "First thing", "number": 51},
        ])
    );
    assert_eq!(v["commit"], code.first.as_str());
    assert_eq!(v["short"], &code.first[..7]);
    assert_eq!(v["app_id"], app);
    assert!(
        v["checked_at"]
            .as_str()
            .is_some_and(|t| t.len() == 20 && t.ends_with('Z')),
        "{v}"
    );
    assert_eq!(
        v["words"],
        "2 changes were merged on GitHub that your PC isn't running yet. Update Decurion to use them."
    );
    assert_eq!(v["how"], json!(decurion_hub::road::update_lines()));
    // The owner's folder is only read: no ref, FETCH_HEAD or object added.
    assert_eq!(code.folder_state(), before);

    // Decurion's row on Today needs the owner and carries the same answer.
    let row = today_row(&h, app).await;
    assert_eq!(row["tone"], "needs_you", "{row}");
    assert_eq!(row["status"], "Needs you", "{row}");
    assert_eq!(row["line"], "Update Decurion · 2 changes waiting", "{row}");
    assert_eq!(row["decurion"], v);

    // Owner only: a runner's key is refused.
    let (s, _) = call(&h.r, "GET", "/api/decurion/version", RUNNER, Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_pc_on_the_newest_decurion_is_current_and_today_shows_nothing_more() {
    let code = Code::new();
    let tip = code.merge("fix: the newest (#9)");
    let h = hub(&code, &tip).await;
    let app = add_app(&h, &code).await;
    let v = version(&h).await;
    assert_eq!(v["state"], "current", "{v}");
    assert_eq!(v["behind"], 0);
    assert_eq!(v["changes"], json!([]));
    assert_eq!(v["words"], "Your PC runs the newest Decurion.");
    let row = today_row(&h, app).await;
    assert!(row.get("decurion").is_none(), "{row}");
    assert_ne!(row["tone"], "needs_you", "{row}");
}

/// The answer is kept for a while; once old, it is still shown at once
/// while a new look runs, and the new answer replaces it.
#[tokio::test]
async fn an_old_answer_is_shown_while_a_new_look_runs() {
    let code = Code::new();
    code.merge("fix: one (#1)");
    let h = hub(&code, &code.first).await;
    add_app(&h, &code).await;
    assert_eq!(version(&h).await["behind"], 1);

    code.merge("fix: two (#2)");
    let (_, v) = call(&h.r, "GET", "/api/decurion/version", OWNER, Value::Null).await;
    assert_eq!(v["behind"], 1, "kept, not asked again: {v}");

    h.state.age_version();
    let (_, v) = call(&h.r, "GET", "/api/decurion/version", OWNER, Value::Null).await;
    assert_eq!(v["behind"], 1, "the old answer at once: {v}");
    let until = Instant::now() + Duration::from_secs(120);
    loop {
        let (_, v) = call(&h.r, "GET", "/api/decurion/version", OWNER, Value::Null).await;
        if v["behind"] == 2 {
            assert_eq!(v["changes"][0]["title"], "Two");
            break;
        }
        assert!(Instant::now() < until, "never looked again: {v}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn when_it_cannot_be_told_the_owner_is_told_why_in_plain_words() {
    // The server is out of reach.
    let code = Code::new();
    code.merge("fix: one (#1)");
    std::fs::rename(&code.origin, code.origin.with_extension("gone")).expect("move origin");
    let h = hub(&code, &code.first).await;
    let app = add_app(&h, &code).await;
    let v = version(&h).await;
    assert_eq!(v["state"], "unknown", "{v}");
    assert_eq!(v["words"], "Couldn't reach GitHub to check.");
    assert_eq!(v["app_id"], app);
    assert_eq!(v["commit"], code.first.as_str());
    assert!(today_row(&h, app).await.get("decurion").is_none());

    // A hub built without knowing its commit.
    let code = Code::new();
    let h = hub(&code, "unknown").await;
    add_app(&h, &code).await;
    let v = version(&h).await;
    assert_eq!(v["state"], "unknown", "{v}");
    assert_eq!(v["commit"], Value::Null);
    assert_eq!(v["short"], Value::Null);
    assert_eq!(
        v["words"],
        "This Decurion doesn't know which version it is."
    );

    // No app is Decurion's own code.
    let code = Code::new();
    let h = hub(&code, &code.first).await;
    let v = version(&h).await;
    assert_eq!(v["state"], "unknown", "{v}");
    assert_eq!(v["app_id"], Value::Null);
    assert!(
        v["words"]
            .as_str()
            .is_some_and(|w| w.starts_with("Add Decurion's own code as an app")),
        "{v}"
    );
}
