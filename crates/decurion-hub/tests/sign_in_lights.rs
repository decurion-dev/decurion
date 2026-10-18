//! The Sign-ins list: one light per account, and the week-ahead reminders.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use decurion_hub::{router, store::Store, AppState, Keys};
use decurion_proto::Profile;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

const OWNER: &str = "owner-test-key";
const RUNNER: &str = "runner-test-key";

struct Hub {
    st: AppState,
    r: axum::Router,
    store: Store,
    vault: decurion_vault::Vault,
    dir: std::path::PathBuf,
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn hub(public_url: Option<&str>) -> Hub {
    let store = Store::open("sqlite::memory:").await.expect("open store");
    let dir = std::env::temp_dir().join(format!(
        "decurion-sign-in-lights-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let vault = decurion_vault::Vault::in_folder(dir.join("vault"));
    let st = AppState::new(
        store.clone(),
        Keys {
            owner: OWNER.into(),
            runner: RUNNER.into(),
        },
        "abc123def456".into(),
        public_url.map(str::to_owned),
    )
    .with_vault(vault.clone());
    let r = router(st.clone());
    Hub {
        st,
        r,
        store,
        vault,
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
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let resp = r.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

async fn lights(r: &axum::Router) -> Value {
    let (s, v) = call(r, "GET", "/api/sign-in-lights", OWNER, Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

fn row<'a>(v: &'a Value, key: &str) -> &'a Value {
    v["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["key"] == key)
        .unwrap_or_else(|| panic!("no {key} in {v}"))
}

#[tokio::test]
async fn a_fresh_hub_shows_what_is_missing_and_what_is_only_not_set_up() {
    let h = hub(None).await;
    let (s, _) = call(
        &h.r,
        "POST",
        "/api/apps",
        OWNER,
        json!({"name":"demo","repo_path":"/repos/demo","profile":"A"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let v = lights(&h.r).await;
    // An app needs Claude A, so its missing sign-in is red; nothing needs B.
    assert_eq!(row(&v, "claude-A")["light"], "red");
    assert_eq!(row(&v, "claude-A")["action"], "claude_sign_in");
    assert_eq!(row(&v, "claude-A")["profile"], "A");
    assert_eq!(row(&v, "claude-B")["light"], "grey");
    assert_eq!(row(&v, "github")["light"], "grey");
    assert_eq!(row(&v, "phone-connection")["action"], "connect_phone");
    assert_eq!(row(&v, "cloud")["action"], "add_cloud");
    assert_eq!(v["red"], 1);
}

#[tokio::test]
async fn a_saved_claude_sign_in_is_green_and_its_date_shows_once_known() {
    let h = hub(Some("https://pc.example.ts.net")).await;
    let token = format!("sk-ant-oat01-{}", "q".repeat(80));
    h.vault.put_claude(Profile::B, &token).expect("save");
    let v = lights(&h.r).await;
    let b = row(&v, "claude-B");
    assert_eq!(b["light"], "green");
    assert!(b["expires_at"].is_null(), "saved another way: date unknown");
    assert!(!v.to_string().contains("sk-ant"), "never shows the sign-in");
    // Phones can already reach the hub.
    assert!(v["items"]
        .as_array()
        .expect("items")
        .iter()
        .all(|i| i["key"] != "phone-connection"));
}

#[tokio::test]
async fn a_phone_about_to_be_signed_out_is_amber_and_reminded_once() {
    let h = hub(None).await;
    h.store.add_device("Android phone").await.expect("device");
    let (id,): (i64,) = sqlx::query_as("SELECT id FROM devices")
        .fetch_one(h.store.pool())
        .await
        .expect("id");
    sqlx::query(
        "UPDATE devices SET last_seen = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-361 days')",
    )
    .execute(h.store.pool())
    .await
    .expect("age");
    let v = lights(&h.r).await;
    let d = row(&v, &format!("device-{id}"));
    assert_eq!(d["light"], "amber", "{d}");
    assert_eq!(d["days_left"], 3);
    assert_eq!(v["amber"], 1);

    assert_eq!(
        decurion_hub::sign_in_lights::remind(&h.st)
            .await
            .expect("remind"),
        1
    );
    assert_eq!(
        decurion_hub::sign_in_lights::remind(&h.st)
            .await
            .expect("remind"),
        0
    );
    // Used again: a new date, so a later reminder could come again.
    sqlx::query("UPDATE devices SET last_seen = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')")
        .execute(h.store.pool())
        .await
        .expect("used");
    assert_eq!(
        row(&lights(&h.r).await, &format!("device-{id}"))["light"],
        "green"
    );
    assert_eq!(
        decurion_hub::sign_in_lights::remind(&h.st)
            .await
            .expect("remind"),
        0
    );
}

#[tokio::test]
async fn only_the_owner_sees_sign_ins() {
    let h = hub(None).await;
    let (s, _) = call(&h.r, "GET", "/api/sign-in-lights", RUNNER, Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}
