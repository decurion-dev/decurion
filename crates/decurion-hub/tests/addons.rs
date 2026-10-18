//! Add-ons, v1 (tools only): the owner adds one from its link with one
//! approval, turns it on per app, and a run of that app gets exactly the
//! approved files; updates ask again only when they widen; removal stops
//! new runs; every step is in the add-on log.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
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
    dir: PathBuf,
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn hub() -> Hub {
    let dir =
        std::env::temp_dir().join(format!("decurion-addons-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let st = AppState::new(
        store.clone(),
        Keys {
            owner: OWNER.into(),
            runner: RUNNER.into(),
        },
        "abc123".into(),
        None,
    )
    .with_files(dir.join("saved"));
    Hub {
        r: router(st),
        store,
        dir,
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

const MANIFEST: &str = r#"decurion_api = "1"

[addon]
id = "example.words"
name = "Word count"
version = "VERSION"
description = "Counts words for agents."

[[tool]]
name = "words"
summary = "Count the words in a piece of text"
command = ["node", "server.mjs"]
effect = "read"

[tool.permissions]
network = [SITES]

[tool.secrets]
WORDS_TOKEN = "WORDS_TOKEN"
"#;

/// The add-on's own repo, in `h.dir`, with one commit.
fn author(h: &Hub) -> PathBuf {
    let repo = h.dir.join("words-addon");
    std::fs::create_dir_all(&repo).expect("repo dir");
    git(&repo, &["init", "--quiet"]);
    publish(&repo, "0.1.0", "\"api.example.com\"", "// v1\n");
    repo
}

fn publish(repo: &Path, version: &str, sites: &str, server: &str) {
    let m = MANIFEST.replace("VERSION", version).replace("SITES", sites);
    std::fs::write(repo.join("decurion-addon.toml"), m).expect("manifest");
    std::fs::write(repo.join("server.mjs"), server).expect("server");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "--quiet", "-m", version]);
}

/// An app with the key the add-on needs, and one queued piece of work.
async fn app_and_work(h: &Hub, name: &str, key: bool) -> (i64, i64) {
    let (s, app) = call(
        &h.r,
        "POST",
        "/api/apps",
        OWNER,
        json!({"name": name, "repo_path": format!("C:\\repos\\{name}"), "profile": "A"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{app}");
    let app = app["id"].as_i64().expect("id");
    if key {
        h.store
            .save_test_key(app, "WORDS_TOKEN", "1234")
            .await
            .expect("key");
    }
    let (s, t) = call(
        &h.r,
        "POST",
        "/api/threads",
        OWNER,
        json!({"app_id": app, "title": "t", "prompt": "do it"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{t}");
    (app, t["id"].as_i64().expect("id"))
}

async fn claim(h: &Hub) -> Value {
    let (s, c) = call(
        &h.r,
        "POST",
        "/api/runner/claim",
        RUNNER,
        json!({"runner": "local", "rules_version": decurion_proto::RULES_VERSION}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    c
}

async fn preview(h: &Hub, repo: &Path) -> (StatusCode, Value) {
    call(
        &h.r,
        "POST",
        "/api/addons/preview",
        OWNER,
        json!({"source": repo.to_string_lossy()}),
    )
    .await
}

async fn events(h: &Hub) -> Vec<String> {
    let (s, v) = call(&h.r, "GET", "/api/addons", OWNER, Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["log"]
        .as_array()
        .expect("log")
        .iter()
        .rev()
        .map(|l| l["event"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[tokio::test]
async fn the_owner_adds_an_add_on_with_one_approval_and_a_run_gets_exactly_its_files() {
    let h = hub().await;
    let repo = author(&h);
    let (app, thread) = app_and_work(&h, "demo", true).await;

    let (s, card) = preview(&h, &repo).await;
    assert_eq!(s, StatusCode::OK, "{card}");
    assert_eq!(card["title"], "Add \"Word count\" (version 0.1.0)?");
    let can: Vec<&str> = card["can"]
        .as_array()
        .expect("can")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(
        can[0],
        "Gives your agents a tool: Count the words in a piece of text."
    );
    assert!(
        can.iter()
            .any(|l| l.starts_with("Talks to api.example.com.")),
        "{can:?}"
    );
    assert!(
        can.iter().any(|l| l.contains("test key WORDS_TOKEN")),
        "{can:?}"
    );
    assert!(card["never"]
        .as_str()
        .expect("never")
        .contains("see your Claude sign-in"));
    assert_eq!(card["rev"].as_str().map(str::len), Some(40));
    assert_eq!(
        card["rev"].as_str(),
        Some(git(&repo, &["rev-parse", "HEAD"]).as_str())
    );
    assert_eq!(card["apps"][0]["missing"], json!([]));
    let hash = card["hash"].as_str().expect("hash").to_owned();
    // Nothing is kept or on until the owner taps Add.
    assert!(h.store.addons().await.expect("addons").is_empty());

    let (s, v) = call(
        &h.r,
        "POST",
        "/api/addons/approve",
        OWNER,
        json!({"hash": hash, "apps": [app]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["version"], "0.1.0");
    // A resent tap changes nothing.
    let (s, again) = call(
        &h.r,
        "POST",
        "/api/addons/approve",
        OWNER,
        json!({"hash": hash, "apps": [app]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{again}");

    let c = claim(&h).await;
    assert_eq!(c["thread"]["id"], thread);
    let a = &c["addons"][0];
    assert_eq!(a["id"], "example.words");
    assert_eq!(a["hash"], hash.as_str());
    assert_eq!(a["tools"][0]["name"], "words");
    assert_eq!(a["tools"][0]["sites"], json!(["api.example.com"]));
    assert_eq!(
        a["tools"][0]["key"],
        json!({"var": "WORDS_TOKEN", "test_key": "WORDS_TOKEN"})
    );
    // The claim names the key; its value is never in it.
    assert!(!c.to_string().contains("1234"));

    let (s, b) = call(
        &h.r,
        "GET",
        &format!("/api/runner/threads/{thread}/addons/example.words"),
        RUNNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let bundle: decurion_proto::addon::Bundle = serde_json::from_value(b).expect("bundle");
    use base64::Engine;
    let files: Vec<(String, Vec<u8>)> = bundle
        .files
        .iter()
        .map(|f| {
            (
                f.path.clone(),
                base64::engine::general_purpose::STANDARD
                    .decode(&f.data)
                    .expect("b64"),
            )
        })
        .collect();
    assert_eq!(
        decurion_proto::addon::tree_hash(files.iter().map(|(p, d)| (p.as_str(), d.as_slice()))),
        hash
    );
    let mut names: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    names.sort();
    assert_eq!(names, ["decurion-addon.toml", "server.mjs"]);

    assert_eq!(
        events(&h).await,
        ["installed", "enabled", "used", "key_used"]
    );
}

#[tokio::test]
async fn only_the_owner_adds_and_a_missing_key_or_a_hard_stop_is_refused_in_plain_words() {
    let h = hub().await;
    let repo = author(&h);
    let (app, _) = app_and_work(&h, "demo", false).await;
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/addons/preview",
        RUNNER,
        json!({"source": repo.to_string_lossy()}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(&h.r, "GET", "/api/addons", RUNNER, Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let (s, card) = preview(&h, &repo).await;
    assert_eq!(s, StatusCode::OK, "{card}");
    assert_eq!(card["apps"][0]["missing"], json!(["WORDS_TOKEN"]));
    let hash = card["hash"].as_str().expect("hash").to_owned();
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/addons/approve",
        RUNNER,
        json!({"hash": hash}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = call(
        &h.r,
        "POST",
        "/api/addons/approve",
        OWNER,
        json!({"hash": hash, "apps": [app]}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(
        v.as_str()
            .expect("text")
            .contains("demo has no test key named WORDS_TOKEN"),
        "{v}"
    );
    // Added for no app, then turning it on still needs the key.
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/addons/approve",
        OWNER,
        json!({"hash": hash}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = call(
        &h.r,
        "POST",
        &format!("/api/addons/example.words/apps/{app}"),
        OWNER,
        json!({"on": true}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(v.as_str().expect("text").contains("WORDS_TOKEN"), "{v}");
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/addons/example.words/apps/{app}"),
        RUNNER,
        json!({"on": true}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // A tool that sends messages, and a link that is not one.
    let sends = h.dir.join("sends");
    std::fs::create_dir_all(&sends).expect("dir");
    git(&sends, &["init", "--quiet"]);
    publish(&sends, "1.0.0", "\"hooks.slack.com\"", "x");
    let (s, v) = preview(&h, &sends).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(
        v.as_str().expect("text").contains("wait for your OK"),
        "{v}"
    );
    let (s, v) = call(
        &h.r,
        "POST",
        "/api/addons/preview",
        OWNER,
        json!({"source": "github.com/x/y"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(v.as_str().expect("text").contains("https link"), "{v}");
}

#[tokio::test]
async fn changed_files_are_refused_before_a_run_uses_them() {
    let h = hub().await;
    let repo = author(&h);
    let (app, thread) = app_and_work(&h, "demo", true).await;
    let (_, card) = preview(&h, &repo).await;
    let hash = card["hash"].as_str().expect("hash").to_owned();
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/addons/approve",
        OWNER,
        json!({"hash": hash, "apps": [app]}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let kept = h
        .dir
        .join("saved")
        .join("addons")
        .join("example.words")
        .join(&hash)
        .join("server.mjs");
    std::fs::write(&kept, "// changed\n").expect("tamper");
    claim(&h).await;
    let (s, v) = call(
        &h.r,
        "GET",
        &format!("/api/runner/threads/{thread}/addons/example.words"),
        RUNNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(
        v.as_str()
            .expect("text")
            .contains("changed after you approved"),
        "{v}"
    );
    assert_eq!(events(&h).await.last().map(String::as_str), Some("refused"));
}

#[tokio::test]
async fn an_update_asking_for_nothing_new_is_applied_and_a_wider_one_waits_for_the_owner() {
    let h = hub().await;
    let repo = author(&h);
    let (app, _) = app_and_work(&h, "demo", true).await;
    let (_, card) = preview(&h, &repo).await;
    let first = card["hash"].as_str().expect("hash").to_owned();
    call(
        &h.r,
        "POST",
        "/api/addons/approve",
        OWNER,
        json!({"hash": first, "apps": [app]}),
    )
    .await;

    let (s, v) = call(
        &h.r,
        "POST",
        "/api/addons/example.words/update",
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(
        (s, v["current"].clone()),
        (StatusCode::OK, json!(true)),
        "{v}"
    );

    // Same permissions: applied at once, no card.
    publish(&repo, "0.2.0", "\"api.example.com\"", "// v2\n");
    let (s, v) = call(
        &h.r,
        "POST",
        "/api/addons/example.words/update",
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["updated"], true);
    let kept = h
        .store
        .addon("example.words")
        .await
        .expect("db")
        .expect("kept");
    assert_eq!(kept.version, "0.2.0");
    let second = kept.hash.clone();
    assert_ne!(second, first);

    // A new website: a card with only what is new, and 0.2.0 keeps running.
    publish(
        &repo,
        "0.3.0",
        "\"api.example.com\", \"files.example.org\"",
        "// v3\n",
    );
    let (s, v) = call(
        &h.r,
        "POST",
        "/api/addons/example.words/update",
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let card = &v["card"];
    assert_eq!(card["title"], "Update \"Word count\" to version 0.3.0?");
    assert_eq!(card["update_from"], "0.2.0");
    let can: Vec<&str> = card["can"]
        .as_array()
        .expect("can")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(can.len(), 1, "{can:?}");
    assert!(can[0].starts_with("Talks to files.example.org."), "{can:?}");
    assert_eq!(
        h.store.claim_addons(app).await.expect("claim addons")[0].hash,
        second
    );
    let third = card["hash"].as_str().expect("hash").to_owned();
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/addons/approve",
        OWNER,
        json!({"hash": third}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let now = h.store.claim_addons(app).await.expect("claim addons");
    assert_eq!(
        (now[0].version.as_str(), now[0].hash.as_str()),
        ("0.3.0", third.as_str())
    );
    // Still on for the app it was on for.
    assert_eq!(
        h.store.addon_apps("example.words").await.expect("apps"),
        [app]
    );
    assert_eq!(
        events(&h).await,
        ["installed", "enabled", "updated", "updated"]
    );
}

#[tokio::test]
async fn turning_off_and_removing_stop_new_runs_and_the_log_stays() {
    let h = hub().await;
    let repo = author(&h);
    let (app, _) = app_and_work(&h, "demo", true).await;
    let (_, card) = preview(&h, &repo).await;
    let hash = card["hash"].as_str().expect("hash").to_owned();
    call(
        &h.r,
        "POST",
        "/api/addons/approve",
        OWNER,
        json!({"hash": hash, "apps": [app]}),
    )
    .await;
    let (s, v) = call(
        &h.r,
        "POST",
        &format!("/api/addons/example.words/apps/{app}"),
        OWNER,
        json!({"on": false}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(h
        .store
        .claim_addons(app)
        .await
        .expect("claim addons")
        .is_empty());
    let (s, _) = call(
        &h.r,
        "POST",
        &format!("/api/addons/example.words/apps/{app}"),
        OWNER,
        json!({"on": true}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        h.store.claim_addons(app).await.expect("claim addons").len(),
        1
    );

    let (s, _) = call(
        &h.r,
        "POST",
        "/api/addons/example.words/remove",
        RUNNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = call(
        &h.r,
        "POST",
        "/api/addons/example.words/remove",
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let c = claim(&h).await;
    assert_eq!(c["addons"], json!([]));
    assert!(!h
        .dir
        .join("saved")
        .join("addons")
        .join("example.words")
        .exists());
    let (_, list) = call(&h.r, "GET", "/api/addons", OWNER, Value::Null).await;
    assert_eq!(list["addons"], json!([]));
    assert_eq!(
        events(&h).await,
        ["installed", "enabled", "disabled", "enabled", "removed"]
    );
    // Gone for runs too.
    let id = c["thread"]["id"].as_i64().expect("id");
    let (s, _) = call(
        &h.r,
        "GET",
        &format!("/api/runner/threads/{id}/addons/example.words"),
        RUNNER,
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

const WORD_COUNT: &str = include_str!("../../../docs/addons/word-count/decurion-addon.toml");

#[tokio::test]
async fn an_add_on_in_a_folder_of_a_repo_is_fetched_from_that_folder_only() {
    let h = hub().await;
    let repo = h.dir.join("many");
    let inside = repo.join("addons").join("words");
    std::fs::create_dir_all(&inside).expect("dirs");
    git(&repo, &["init", "--quiet"]);
    std::fs::write(repo.join("README.md"), "not part of the add-on").expect("readme");
    std::fs::write(inside.join("decurion-addon.toml"), WORD_COUNT).expect("manifest");
    std::fs::write(inside.join("server.mjs"), "// server\n").expect("server");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "-m", "one"]);
    let source = repo.to_string_lossy().into_owned();
    let (s, card) = call(
        &h.r,
        "POST",
        "/api/addons/preview",
        OWNER,
        json!({"source": source, "folder": "addons/words"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{card}");
    assert_eq!(card["id"], "decurion.word-count");
    let files = [
        ("decurion-addon.toml", WORD_COUNT.as_bytes()),
        ("server.mjs", b"// server\n".as_slice()),
    ];
    assert_eq!(card["hash"], decurion_proto::addon::tree_hash(files));
    let (s, v) = call(
        &h.r,
        "POST",
        "/api/addons/preview",
        OWNER,
        json!({"source": source, "folder": "../etc"}),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
}

#[tokio::test]
async fn the_add_ons_tab_is_served_and_both_pages_load_it() {
    let h = hub().await;
    let resp =
        h.r.clone()
            .oneshot(Request::get("/addons.js").body(Body::empty()).expect("req"))
            .await
            .expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(ct.starts_with("text/javascript"), "{ct}");
    for page in [
        include_str!("../src/board.html"),
        include_str!("../src/inbox.html"),
    ] {
        let a = page
            .find("<script src=\"/addons.js\">")
            .expect("loads addons.js");
        let s = page
            .find("<script src=\"/settings.js\">")
            .expect("loads settings.js");
        assert!(a < s, "addons.js loads before settings.js builds its tabs");
    }
}
