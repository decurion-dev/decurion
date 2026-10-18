//! The folder picker: only folder names, only inside the safe starting
//! places, and only for the owner.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::folders::Places;
use decurion_hub::{router, store::Store, AppState, Keys};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";

/// A made-up PC in a temp folder:
///
/// ```text
/// <tmp>/home/Projects/portal/.git/      a git app folder
/// <tmp>/home/Projects/portal/src/
/// <tmp>/home/Projects/notes/            a plain folder
/// <tmp>/home/Projects/readme.txt        a file, never shown
/// <tmp>/home/Projects/.secret/          a dot folder, never shown
/// <tmp>/home/Projects/node_modules/     clutter, never shown
/// <tmp>/home/Projects/decurion-data/    the hub's own data, never shown
/// <tmp>/home/Documents/
/// <tmp>/home/private/                   not a starting place
/// <tmp>/elsewhere/                      outside the home folder
/// ```
struct Pc {
    tmp: PathBuf,
    home: PathBuf,
    r: axum::Router,
}

impl Drop for Pc {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

impl Pc {
    fn projects(&self) -> PathBuf {
        self.home.join("Projects")
    }
}

fn mkdir(p: &Path) {
    std::fs::create_dir_all(p).expect("make test folder");
}

async fn pc() -> Pc {
    let tmp = std::env::temp_dir().join(format!("decurion-folders-{}", uuid::Uuid::new_v4()));
    let home = tmp.join("home");
    let projects = home.join("Projects");
    for d in [
        projects.join("portal").join(".git"),
        projects.join("portal").join("src"),
        projects.join("notes"),
        projects.join(".secret"),
        projects.join("node_modules"),
        projects.join("decurion-data"),
        home.join("Documents"),
        home.join("private"),
        tmp.join("elsewhere").join("app").join(".git"),
    ] {
        mkdir(&d);
    }
    std::fs::write(projects.join("readme.txt"), "hello").expect("write file");
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let places = Places::new(
        Some(home.clone()),
        vec![],
        vec![projects.join("decurion-data")],
    );
    let r = router(
        AppState::new(
            store,
            Keys {
                owner: OWNER.into(),
                runner: RUNNER.into(),
            },
            "abc123".into(),
            None,
        )
        .with_folders(places),
    );
    Pc { tmp, home, r }
}

async fn call(
    r: &axum::Router,
    method: &str,
    path: &str,
    key: &str,
    body: Value,
) -> (StatusCode, Value) {
    let b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {key}"));
    let body = if method == "GET" {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let resp = r
        .clone()
        .oneshot(b.body(body).expect("request"))
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

/// `GET /api/folders?path=<p>`, with the path escaped for the address.
async fn open(r: &axum::Router, p: &str, key: &str) -> (StatusCode, Value) {
    let q: String = url::form_urlencoded::byte_serialize(p.as_bytes()).collect();
    call(
        r,
        "GET",
        &format!("/api/folders?path={q}"),
        key,
        Value::Null,
    )
    .await
}

fn names(v: &Value) -> Vec<&str> {
    v["folders"]
        .as_array()
        .expect("folders")
        .iter()
        .map(|f| f["name"].as_str().expect("name"))
        .collect()
}

fn path_of<'a>(v: &'a Value, name: &str) -> &'a str {
    v["folders"]
        .as_array()
        .expect("folders")
        .iter()
        .find(|f| f["name"] == name)
        .and_then(|f| f["path"].as_str())
        .unwrap_or_else(|| panic!("no folder named {name} in {v}"))
}

#[tokio::test]
async fn the_starting_places_are_the_usual_code_folders_that_exist() {
    let pc = pc().await;
    let (s, v) = call(&pc.r, "GET", "/api/folders", OWNER, Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(names(&v), ["Documents", "Projects"]);
    assert!(v["path"].is_null() && v["up"].is_null(), "{v}");
    assert_eq!(v["cut"], false);
    let p = path_of(&v, "Projects");
    assert!(
        Path::new(p).is_absolute() && Path::new(p).ends_with("Projects"),
        "{p}"
    );
    assert!(!p.starts_with(r"\\?\"), "plain path for display: {p}");
}

#[tokio::test]
async fn sub_folders_are_listed_with_git_folders_marked_and_no_files() {
    let pc = pc().await;
    let (_, start) = call(&pc.r, "GET", "/api/folders", OWNER, Value::Null).await;
    let projects = path_of(&start, "Projects").to_owned();
    let (s, v) = open(&pc.r, &projects, OWNER).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // No file, dot folder, clutter or the hub's own data.
    assert_eq!(names(&v), ["notes", "portal"]);
    let git: Vec<bool> = v["folders"]
        .as_array()
        .expect("folders")
        .iter()
        .map(|f| f["git"].as_bool().expect("git"))
        .collect();
    assert_eq!(git, [false, true]);
    assert_eq!(v["path"], projects.as_str());
    assert_eq!(v["git"], false);
    assert!(
        v["up"].is_null(),
        "Up from a starting place goes to the list"
    );
    assert_eq!(v["crumbs"].as_array().expect("crumbs").len(), 1);

    let portal = path_of(&v, "portal").to_owned();
    let (s, v) = open(&pc.r, &portal, OWNER).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["git"], true);
    assert_eq!(v["name"], "portal");
    assert_eq!(names(&v), ["src"], ".git itself is not listed");
    assert_eq!(v["up"], projects.as_str());
    let crumbs: Vec<&str> = v["crumbs"]
        .as_array()
        .expect("crumbs")
        .iter()
        .map(|c| c["name"].as_str().expect("name"))
        .collect();
    assert_eq!(crumbs, ["Projects", "portal"]);
}

#[tokio::test]
async fn folders_outside_the_starting_places_are_refused() {
    let pc = pc().await;
    let projects = pc.projects();
    let refused = |p: PathBuf| p.to_str().expect("text path").to_owned();
    for p in [
        // Climbing out with "..".
        projects.join("..").join("private"),
        projects.join("portal").join("..").join("..").join(".."),
        // The whole home folder, a folder in it, the drive root, another place.
        pc.home.clone(),
        pc.home.join("private"),
        pc.home.ancestors().last().expect("a root").to_owned(),
        pc.tmp.join("elsewhere"),
    ] {
        let (s, v) = open(&pc.r, &refused(p.clone()), OWNER).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{} gave {v}", p.display());
        assert!(
            v.as_str()
                .is_some_and(|t| t.starts_with("Decurion only shows folders inside")),
            "{v}"
        );
    }
    // Decurion's own data, even though it sits inside Projects.
    let (s, v) = open(&pc.r, &refused(projects.join("decurion-data")), OWNER).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(
        v,
        "That folder holds Decurion's own data, so Decurion does not show it."
    );
    // Folders the list never shows cannot be opened by typing them either.
    for hidden in [".secret", "node_modules"] {
        let (s, _) = open(&pc.r, &refused(projects.join(hidden)), OWNER).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{hidden}");
    }
    // A file, a missing folder and a relative path.
    for p in [
        refused(projects.join("readme.txt")),
        refused(projects.join("gone")),
        "Projects".to_owned(),
    ] {
        let (s, v) = open(&pc.r, &p, OWNER).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{p} gave {v}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_link_out_of_the_starting_places_is_neither_listed_nor_followed() {
    let pc = pc().await;
    let link = pc.projects().join("shortcut");
    std::os::unix::fs::symlink(pc.home.join("private"), &link).expect("make link");
    let (_, v) = open(&pc.r, pc.projects().to_str().expect("text"), OWNER).await;
    assert_eq!(names(&v), ["notes", "portal"]);
    let (s, v) = open(&pc.r, link.to_str().expect("text"), OWNER).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
}

#[tokio::test]
async fn the_folder_holding_an_app_becomes_a_starting_place() {
    let pc = pc().await;
    let app = pc.tmp.join("elsewhere").join("app");
    let (s, a) = call(
        &pc.r,
        "POST",
        "/api/apps",
        OWNER,
        json!({"name":"other","repo_path": app.to_str().expect("text"),"profile":"A"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    let (_, v) = call(&pc.r, "GET", "/api/folders", OWNER, Value::Null).await;
    assert_eq!(names(&v), ["Documents", "elsewhere", "Projects"]);
    let (s, v) = open(&pc.r, path_of(&v, "elsewhere"), OWNER).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(names(&v), ["app"]);
    assert_eq!(v["folders"][0]["git"], true);
}

#[tokio::test]
async fn an_app_in_the_home_folder_does_not_open_the_whole_home_folder() {
    let pc = pc().await;
    let app = pc.home.join("private");
    let (s, _) = call(
        &pc.r,
        "POST",
        "/api/apps",
        OWNER,
        json!({"name":"homeapp","repo_path": app.to_str().expect("text"),"profile":"A"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (_, v) = call(&pc.r, "GET", "/api/folders", OWNER, Value::Null).await;
    assert_eq!(names(&v), ["Documents", "Projects"]);
    let (s, _) = open(&pc.r, pc.home.to_str().expect("text"), OWNER).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn only_the_owner_browses_folders() {
    let pc = pc().await;
    let (s, _) = call(&pc.r, "GET", "/api/folders", RUNNER, Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = open(&pc.r, pc.projects().to_str().expect("text"), RUNNER).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // A cloud worker's key is refused before the handler runs.
    let (s, j) = call(&pc.r, "POST", "/api/workers/join-code", OWNER, Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{j}");
    let (s, reply) = call(
        &pc.r,
        "POST",
        "/runner/join",
        "unused",
        json!({ "code": j["code"] }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{reply}");
    let token = reply["token"].as_str().expect("token");
    let (s, v) = call(&pc.r, "GET", "/api/folders", token, Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(v, "cloud workers only run their own work");
}

#[tokio::test]
async fn a_hub_without_the_picker_says_to_type_the_path() {
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let r = router(AppState::new(
        store,
        Keys {
            owner: OWNER.into(),
            runner: RUNNER.into(),
        },
        "abc123".into(),
        None,
    ));
    let (s, v) = call(&r, "GET", "/api/folders", OWNER, Value::Null).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        v.as_str()
            .is_some_and(|t| t.contains("Type the folder's path")),
        "{v}"
    );
}
