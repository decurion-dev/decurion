//! The Decurion hub: holds board state and serves the owner's board, the
//! phone inbox and the runner API.
//!
//! Every `/api` call needs a key. Agents run in containers that can reach the
//! host network, so an unauthenticated API would let an agent move its own
//! card to Done. Containers never receive either key.
//!
//! Browsers and phones never hold the owner key. The PC browser signs in with
//! a login code the hub prints at start; a phone signs in by scanning a
//! short-lived pairing QR code on the board. Either way the browser gets its
//! own device token in an HttpOnly cookie, which survives hub restarts and can
//! be removed from the board.
//!
//! Runners sign in with their own keys: the PC runner with the runner key, a
//! cloud worker with the key it got when it signed up. The hub knows which
//! runner is calling from its key alone, and a runner can only report on,
//! save or move work it is running itself. A cloud worker's key reaches
//! nothing else at all: not the board, spending or other work.

pub mod browser;
pub mod checkpoints;
pub mod context;
pub mod coordinator;
pub mod devices;
pub mod folders;
pub mod github;
pub mod limits;
pub mod messages;
pub mod protected;
pub mod push;
pub mod secrets;
pub mod sign_ins;
pub mod spend;
pub mod store;
pub mod test_keys;
pub mod usage;
pub mod workers;

mod goals;

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use checkpoints::{Files, Part};
use decurion_proto::{
    HelloReply, JoinReply, JoinRequest, MoveRequest, NewApp, NewCheckpoint, NewMessage, NewThread,
    Place, PlaceRequest, RunEvent, RunnerHello, RULES_VERSION,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use store::{Store, StoreError};
use workers::RunnerId;

/// The git commit this hub was built from. A cloud worker builds the same
/// commit, so both computers run the same rules.
pub const HUB_COMMIT: &str = match option_env!("DECURION_COMMIT") {
    Some(c) => c,
    None => "unknown",
};

/// The public repository cloud workers build from.
pub const REPO_URL: &str = "https://github.com/decurion-dev/decurion";

/// A runner counts as online if it was heard from this recently. Runners ask
/// for work every few seconds.
const ONLINE_SECS: u64 = 30;

/// Who is calling. The owner uses the board or a paired phone; the runner
/// uses its own key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caller {
    Owner,
    Runner,
}

impl Caller {
    fn actor(self) -> &'static str {
        match self {
            Caller::Owner => store::OWNER,
            Caller::Runner => "runner",
        }
    }
}

/// The signed-in browser or phone making an owner call, if it came with a
/// device cookie rather than a bearer key.
#[derive(Clone, Copy, Debug)]
pub struct DeviceId(pub Option<i64>);

#[derive(Clone)]
pub struct Keys {
    pub owner: String,
    pub runner: String,
}

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub keys: Keys,
    login_code: Arc<str>,
    /// Address phones use to reach the hub, e.g. the Tailscale https name.
    public_url: Option<Arc<str>>,
    pub(crate) push: Option<push::Pusher>,
    /// Where apps' test keys are kept. Without one, the key endpoints refuse.
    vault: Option<decurion_vault::Vault>,
    files: Option<Arc<Files>>,
    /// Sends accepted work to GitHub. Without it, accepted work waits.
    github: Option<Arc<github::GitHub>>,
    /// When each runner was last heard from, by name.
    seen: Arc<Mutex<HashMap<String, Instant>>>,
    /// Live views of test browsers going on now.
    pub(crate) browser: browser::Relays,
    /// Where the folder picker may look. Without it, the picker refuses.
    pub(crate) folders: Option<Arc<folders::Places>>,
}

impl AppState {
    /// `login_code` is printed for the owner at start and signs in browsers
    /// until the hub restarts. `public_url` goes into pairing QR codes.
    pub fn new(store: Store, keys: Keys, login_code: String, public_url: Option<String>) -> Self {
        Self {
            store,
            keys,
            login_code: login_code.into(),
            public_url: public_url.map(|u| u.trim_end_matches('/').into()),
            push: None,
            vault: None,
            files: None,
            github: None,
            seen: Arc::default(),
            browser: browser::Relays::default(),
            folders: None,
        }
    }

    /// Turns on the folder picker, limited to the places in `p`.
    pub fn with_folders(mut self, p: folders::Places) -> Self {
        self.folders = Some(Arc::new(p));
        self
    }

    /// Where saved copies of work are kept. Without it, work cannot move
    /// between computers.
    pub fn with_files(mut self, dir: std::path::PathBuf) -> Self {
        self.files = Some(Arc::new(Files::new(dir)));
        self
    }

    /// Turns on phone notifications.
    pub fn with_push(mut self, p: push::Pusher) -> Self {
        self.push = Some(p);
        self
    }

    /// Turns on test keys, kept in `v`.
    pub fn with_vault(mut self, v: decurion_vault::Vault) -> Self {
        self.vault = Some(v);
        self
    }

    /// Turns on sending accepted work to GitHub. Needs [`AppState::with_files`]
    /// too, since the work is sent from the hub's saved copy.
    pub fn with_github(mut self, g: github::GitHub) -> Self {
        self.github = Some(Arc::new(g));
        self
    }

    /// What sending to GitHub needs, if this hub is set up for it.
    pub(crate) fn sender(&self) -> Option<github::Sender> {
        Some(github::Sender {
            store: self.store.clone(),
            files: self.files.clone()?,
            gh: self.github.clone()?,
            push: self.push.clone(),
        })
    }
}

const COOKIE: &str = "decurion_device";

/// Writes a file so that a crash mid-write never leaves a half-written one.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Random 256-bit token.
pub fn new_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

const CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'";

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/apps", get(list_apps).post(add_app))
        .route("/threads", get(list_threads).post(add_thread))
        .route("/threads/{id}", get(get_thread))
        .route("/threads/{id}/move", post(move_thread))
        .route("/threads/{id}/events", get(list_events).post(add_event))
        .route(
            "/threads/{id}/messages",
            get(list_messages).post(add_message),
        )
        .route("/threads/{id}/messages/delivered", post(messages_delivered))
        .route("/threads/{id}/live", get(live))
        .route("/runner/hello", post(runner_hello))
        .route("/runner/claim", post(runner_claim))
        .route("/pairing", post(new_pairing))
        .route("/devices", get(list_devices))
        .route("/devices/{id}/remove", post(remove_device))
        .route("/push/key", get(push_key))
        .route("/spend", get(get_spend))
        .route("/limits", get(get_limits))
        .route("/limits/{profile}/pay-per-use", post(overflow))
        .route("/spend/cap", post(set_cap))
        .route("/apps/{id}/pay-per-use", post(set_pay_per_use))
        .route("/apps/{id}/sites", post(set_sites))
        .route("/sites/built-in", get(built_in_sites))
        .route("/apps/{id}/cap", post(set_app_cap))
        .route("/accounts", get(list_accounts))
        // The folder picker: owner only, never cloud workers (folders.rs).
        .route("/folders", get(folders::list))
        .route(
            "/apps/{id}/test-keys",
            get(list_test_keys).post(add_test_key),
        )
        .route("/apps/{id}/test-keys/{name}/remove", post(remove_test_key))
        .route("/push/subscribe", post(push_subscribe))
        .route("/threads/{id}/place", post(set_place))
        .route("/threads/{id}/github", post(github::retry))
        .route("/github", get(github::settings))
        .route("/github/token", post(github::save_token))
        .route("/github/token/remove", post(github::remove_token))
        .route("/workers", get(list_workers))
        .route("/workers/join-code", post(new_join_code))
        .route("/workers/{id}/remove", post(remove_worker))
        .route("/runner/threads/{id}/base", get(runner_base))
        .route(
            "/runner/threads/{id}/upload/{part}",
            put(runner_upload).layer(DefaultBodyLimit::disable()),
        )
        .route("/runner/threads/{id}/checkpoint", post(runner_checkpoint))
        .route(
            "/runner/threads/{id}/checkpoint/{cp}/{part}",
            get(runner_download),
        )
        .route("/runner/threads/{id}/credential", post(runner_credential))
        .route("/goals", get(goals::list).post(goals::add))
        .route("/goals/{id}", get(goals::view))
        .route("/goals/{id}/say", post(goals::say))
        .route("/goals/{id}/answer", post(goals::answer))
        .route("/goals/{id}/approve", post(goals::approve))
        .route("/goals/{id}/retry", post(goals::retry))
        .route("/goals/{id}/close", post(goals::close))
        .route("/goals/{id}/stop", post(goals::stop))
        .route("/goals/{id}/pages/{page}", get(goals::page))
        .route("/apps/{id}/notes", get(goals::notes))
        .route("/apps/{id}/notes/{note}/remove", post(goals::remove_note))
        .route("/runner/helper/claim", post(goals::helper_claim))
        .route("/runner/helper/{id}/started", post(goals::helper_started))
        // Watching and taking over the test browser: see browser.rs.
        .merge(browser::routes())
        .route(
            "/runner/helper/{id}/finish",
            // Room for a page of screenshots and a long answer, escaped inside JSON.
            post(goals::helper_finish).layer(axum::extract::DefaultBodyLimit::max(
                decurion_proto::goal::MAX_PAGE_BYTES * 2 + (1 << 20),
            )),
        )
        // Files the owner attaches for agents (context.rs).
        .route(
            "/apps/{id}/files",
            get(context::list).post(context::upload).layer(
                // A little over the largest file; the handler gives the owner
                // plain words at the largest file itself.
                DefaultBodyLimit::max(
                    usize::try_from(decurion_proto::context::MAX_FILE_BYTES).unwrap_or(usize::MAX)
                        + (1 << 20),
                ),
            ),
        )
        .route("/apps/{id}/files/{file}", get(context::download))
        .route("/apps/{id}/files/{file}/remove", post(context::remove))
        .route("/runner/threads/{id}/context", get(context::runner_thread))
        // Not in cloud_reach: helpers run on the PC only.
        .route("/runner/helper/{id}/context", get(context::runner_helper))
        // Saved website sign-ins: owner and runner routes (sign_ins.rs).
        .merge(sign_ins::routes())
        .layer(middleware::from_fn_with_state(state.clone(), require_key));
    Router::new()
        .route("/", get(board))
        .route("/inbox", get(inbox))
        .route("/cloud", get(cloud_page))
        .route("/runner/join", post(runner_join))
        .route("/goals", get(goals_page))
        .route("/login", get(login_page).post(login))
        .route("/pair", get(pair_page).post(pair))
        .route("/sw.js", get(service_worker))
        .route("/manifest.webmanifest", get(manifest))
        .route(
            "/ui.css",
            get(|| async { asset("text/css; charset=utf-8", include_str!("ui.css")) }),
        )
        .route(
            "/ui.js",
            get(|| async { asset("text/javascript; charset=utf-8", include_str!("ui.js")) }),
        )
        // The new-work form and the settings, shared by the board and the phone.
        .route(
            "/compose.js",
            get(|| async { asset("text/javascript; charset=utf-8", include_str!("compose.js")) }),
        )
        .route(
            "/settings.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("settings.js"),
                )
            }),
        )
        .route(
            "/files.js",
            get(|| async { asset("text/javascript; charset=utf-8", include_str!("files.js")) }),
        )
        .route(
            "/voice.js",
            get(|| async { asset("text/javascript; charset=utf-8", include_str!("voice.js")) }),
        )
        .route(
            "/icon.svg",
            get(|| async { asset("image/svg+xml", include_str!("icon.svg")) }),
        )
        .route(
            "/icon-192.png",
            get(|| async { asset("image/png", &include_bytes!("icon-192.png")[..]) }),
        )
        .route(
            "/icon-512.png",
            get(|| async { asset("image/png", &include_bytes!("icon-512.png")[..]) }),
        )
        .route("/healthz", get(|| async { "ok" }))
        .nest("/api", api)
        .with_state(state)
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

fn device_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|c| {
            c.split(';')
                .filter_map(|p| p.trim().split_once('='))
                .find(|(k, _)| *k == COOKIE)
                .map(|(_, v)| v.to_owned())
        })
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Cookies are sent by any page on the same host, whatever its port, so a
/// cookie-authenticated write must come from the hub's own pages. Browsers say
/// where a request came from in `Sec-Fetch-Site` (or, older ones, `Origin`).
fn same_origin_write(req: &Request) -> bool {
    use axum::http::Method;
    if matches!(*req.method(), Method::GET | Method::HEAD) {
        return true;
    }
    let h = req.headers();
    if let Some(site) = h.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        return site == "same-origin";
    }
    let host = h.get(header::HOST).and_then(|v| v.to_str().ok());
    let origin = h.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    match (host, origin) {
        (Some(host), Some(origin)) => origin
            .split_once("://")
            .is_some_and(|(_, rest)| rest == host),
        _ => false,
    }
}

async fn require_key(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let (caller, device) = match (bearer(req.headers()), device_cookie(req.headers())) {
        (Some(k), _) if same(k, &st.keys.owner) => (Caller::Owner, None),
        (Some(k), _) if same(k, &st.keys.runner) => {
            req.extensions_mut()
                .insert(st.heard_from(RunnerId::local()));
            (Caller::Runner, None)
        }
        (Some(k), _) => match st.store.cloud_runner_for_token(k).await {
            Ok(Some(r)) => {
                let r = st.heard_from(r);
                let reach = cloud_reach(req.method(), req.uri().path());
                if let Err(resp) = cloud_may_call(&st, &r, reach).await {
                    return resp;
                }
                req.extensions_mut().insert(r);
                (Caller::Runner, None)
            }
            Ok(None) => return (StatusCode::UNAUTHORIZED, "missing or wrong key").into_response(),
            Err(e) => return ApiError(e).into_response(),
        },
        (None, Some(_)) if !same_origin_write(&req) => {
            return (StatusCode::FORBIDDEN, "cross-site request refused").into_response()
        }
        (None, Some(t)) => match st.store.device_for_token(&t).await {
            Ok(Some(id)) => (Caller::Owner, Some(id)),
            Ok(None) => return (StatusCode::UNAUTHORIZED, "signed out").into_response(),
            Err(e) => return ApiError(e).into_response(),
        },
        _ => return (StatusCode::UNAUTHORIZED, "missing or wrong key").into_response(),
    };
    req.extensions_mut().insert(caller);
    req.extensions_mut().insert(DeviceId(device));
    next.run(req).await
}

/// What a cloud worker's call reaches.
enum CloudReach {
    Any,
    /// Work it must be running now. `hidden` answers "not found" otherwise,
    /// so a worker cannot learn about other work.
    Thread {
        id: i64,
        hidden: bool,
    },
    Refused,
}

/// A cloud worker is a rented server, not the owner's PC, so its key reaches
/// only what running its own work needs. Anything not listed here, including
/// routes added later, is refused.
fn cloud_reach(method: &axum::http::Method, path: &str) -> CloudReach {
    use axum::http::Method;
    let path = path.strip_prefix("/api").unwrap_or(path);
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    let thread = |id: &str, hidden| match id.parse() {
        Ok(id) => CloudReach::Thread { id, hidden },
        Err(_) => CloudReach::Refused,
    };
    match (method, parts.as_slice()) {
        (&Method::POST, ["runner", "hello" | "claim"]) => CloudReach::Any,
        (&Method::GET, ["threads", id] | ["threads", id, "live"]) => thread(id, true),
        (
            &Method::POST,
            ["threads", id, "events" | "move"] | ["threads", id, "messages", "delivered"],
        ) => thread(id, false),
        (_, ["runner", "threads", id, _, ..]) => thread(id, false),
        // The relay checks the session is this worker's own.
        (&Method::GET, ["runner", "browser", _, "ws"]) => CloudReach::Any,
        _ => CloudReach::Refused,
    }
}

// Responses are what axum handlers return; boxing them buys nothing here.
#[allow(clippy::result_large_err)]
async fn cloud_may_call(st: &AppState, r: &RunnerId, reach: CloudReach) -> Result<(), Response> {
    let (id, hidden) = match reach {
        CloudReach::Any => return Ok(()),
        CloudReach::Thread { id, hidden } => (id, hidden),
        CloudReach::Refused => {
            return Err((
                StatusCode::FORBIDDEN,
                "cloud workers only run their own work",
            )
                .into_response())
        }
    };
    match st.store.running_on(id, &r.name).await {
        Ok(true) => Ok(()),
        Ok(false) if hidden => Err(ApiError(StoreError::NotFound).into_response()),
        Ok(false) => Err((
            StatusCode::CONFLICT,
            "that work is not running on this runner",
        )
            .into_response()),
        Err(e) => Err(ApiError(e).into_response()),
    }
}

impl AppState {
    /// Notes that a runner called, for the board's online dot.
    fn heard_from(&self, r: RunnerId) -> RunnerId {
        if let Ok(mut seen) = self.seen.lock() {
            seen.insert(r.name.clone(), Instant::now());
        }
        r
    }

    fn seconds_ago(&self, name: &str) -> Option<u64> {
        self.seen
            .lock()
            .ok()
            .and_then(|s| s.get(name).map(|t| t.elapsed().as_secs()))
    }
}

pub struct ApiError(StoreError);

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = match &self.0 {
            StoreError::NotFound => StatusCode::NOT_FOUND,
            StoreError::BadMove { .. }
            | StoreError::Conflict
            | StoreError::Duplicate(_)
            | StoreError::Refused(_) => StatusCode::CONFLICT,
            StoreError::Invalid(_) => StatusCode::BAD_REQUEST,
            StoreError::Db(e) => {
                tracing::error!(error = %e, "database error");
                return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
            }
        };
        (code, self.0.to_string()).into_response()
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

pub(crate) fn forbidden(what: &str) -> Response {
    (StatusCode::FORBIDDEN, format!("{what} is owner-only")).into_response()
}

fn not_runner(what: &str) -> Response {
    (StatusCode::FORBIDDEN, format!("{what} is runner-only")).into_response()
}

/// 201 for something new, 200 for the answer to a resent receipt.
fn created_or_resent<T: serde::Serialize>(value: T, fresh: bool) -> Response {
    let code = if fresh {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    (code, Json(value)).into_response()
}

fn page(html: impl Into<String>) -> Response {
    (
        [
            (header::CONTENT_SECURITY_POLICY, CSP),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CACHE_CONTROL, "no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        Html(html.into()),
    )
        .into_response()
}

/// A one-button page. Opening a sign-in link only shows this: link previews
/// and prefetchers open links too, and must never get signed in.
fn confirm_page(action: &str, code: &str, title: &str, text: &str, button: &str) -> Response {
    // Codes are hex; anything else is not ours and is not echoed back.
    if code.is_empty() || !code.bytes().all(|b| b.is_ascii_hexdigit()) {
        return (
            StatusCode::BAD_REQUEST,
            Html("This is not a Decurion sign-in link."),
        )
            .into_response();
    }
    page(
        include_str!("confirm.html")
            .replace("{{action}}", action)
            .replace("{{code}}", code)
            .replace("{{title}}", title)
            .replace("{{text}}", text)
            .replace("{{button}}", button),
    )
}

async fn board() -> Response {
    page(include_str!("board.html"))
}

async fn inbox() -> Response {
    page(include_str!("inbox.html"))
}

async fn goals_page() -> Response {
    page(include_str!("goals.html"))
}

/// Sets the device cookie. `Secure` whenever the hub is reached over https
/// (Tailscale serve sets `X-Forwarded-Proto`).
fn sign_in(headers: &HeaderMap, token: &str, to: &str) -> Response {
    let https = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("https"));
    let secure = if https { "; Secure" } else { "" };
    let cookie =
        format!("{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=31536000{secure}");
    ([(header::SET_COOKIE, cookie)], Redirect::to(to)).into_response()
}

#[derive(Deserialize)]
struct CodeQuery {
    code: String,
}

async fn login_page(Query(q): Query<CodeQuery>) -> Response {
    confirm_page(
        "/login",
        &q.code,
        "Sign in to Decurion",
        "This browser will see your board and can approve work. Only do this on your own PC.",
        "Sign in",
    )
}

/// The "Sign in" button on the link printed by the hub at start. Signs this
/// browser in as a device; the owner key itself never reaches the browser.
async fn login(
    State(st): State<AppState>,
    headers: HeaderMap,
    axum::Form(q): axum::Form<CodeQuery>,
) -> Response {
    if !same(&q.code, &st.login_code) {
        return (StatusCode::UNAUTHORIZED, "wrong or expired login code").into_response();
    }
    match st.store.add_device("Browser on this PC").await {
        Ok((_, token)) => sign_in(&headers, &token, "/"),
        Err(e) => ApiError(e).into_response(),
    }
}

/// A short name for a newly paired device, from its browser's description.
fn device_name(headers: &HeaderMap) -> &'static str {
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if ua.contains("Android") {
        "Android phone"
    } else if ua.contains("iPhone") {
        "iPhone"
    } else if ua.contains("iPad") {
        "iPad"
    } else {
        "Browser"
    }
}

/// Where a scanned pairing QR code lands.
async fn pair_page(Query(q): Query<CodeQuery>) -> Response {
    confirm_page(
        "/pair",
        &q.code,
        "Pair this phone",
        "This phone will get your Decurion inbox and can approve work. Only do this on your own phone.",
        "Pair this phone",
    )
}

/// The "Pair this phone" button.
async fn pair(
    State(st): State<AppState>,
    headers: HeaderMap,
    axum::Form(q): axum::Form<CodeQuery>,
) -> Response {
    match st.store.pair_device(&q.code, device_name(&headers)).await {
        Ok(Some(token)) => sign_in(&headers, &token, "/inbox"),
        Ok(None) => (
            StatusCode::UNAUTHORIZED,
            Html("This pairing code is used or expired. Make a new one on the board."),
        )
            .into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

#[derive(serde::Serialize)]
struct Pairing {
    url: String,
    qr_svg: String,
    minutes: i64,
    /// Set when the phone could not get notifications through this address.
    warning: Option<&'static str>,
}

async fn new_pairing(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    headers: HeaderMap,
) -> Response {
    if c != Caller::Owner {
        return forbidden("pairing a phone");
    }
    let base = match &st.public_url {
        Some(u) => u.to_string(),
        None => {
            let host = headers
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("127.0.0.1:7420");
            format!("http://{host}")
        }
    };
    let code = match st.store.new_pairing_code().await {
        Ok(c) => c,
        Err(e) => return ApiError(e).into_response(),
    };
    let url = format!("{base}/pair?code={code}");
    let qr_svg = match qrcode::QrCode::new(url.as_bytes()) {
        Ok(q) => q
            .render::<qrcode::render::svg::Color>()
            .min_dimensions(220, 220)
            .build(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let warning = (!url.starts_with("https://")).then_some(
        "This address is not https, so the phone cannot get notifications and may not reach it at all. Start the hub with DECURION_PUBLIC_URL set to its Tailscale https address.",
    );
    Json(Pairing {
        url,
        qr_svg,
        minutes: devices::PAIRING_MINUTES,
        warning,
    })
    .into_response()
}

/// A paired device as Settings lists it: `this_device` marks the phone or
/// browser asking, so Settings can say "This phone" next to it.
#[derive(serde::Serialize)]
struct DeviceRow {
    #[serde(flatten)]
    device: devices::Device,
    this_device: bool,
}

async fn list_devices(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    axum::Extension(DeviceId(me)): axum::Extension<DeviceId>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("listing devices");
    }
    match st.store.devices().await {
        Ok(d) => Json(
            d.into_iter()
                .map(|device| DeviceRow {
                    this_device: me == Some(device.id),
                    device,
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn remove_device(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Path(id): Path<i64>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("removing devices");
    }
    match st.store.remove_device(id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn list_apps(State(st): State<AppState>) -> ApiResult<Vec<decurion_proto::App>> {
    Ok(Json(st.store.apps().await?))
}

async fn add_app(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Json(a): Json<NewApp>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("adding apps");
    }
    match st.store.add_app(&a).await {
        Ok(app) => (StatusCode::CREATED, Json(app)).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn list_threads(State(st): State<AppState>) -> ApiResult<Vec<decurion_proto::Thread>> {
    Ok(Json(st.store.threads().await?))
}

async fn get_thread(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<decurion_proto::Thread> {
    Ok(Json(st.store.thread(id).await?))
}

async fn add_thread(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Json(t): Json<NewThread>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("adding work");
    }
    match st.store.add_thread(&t).await {
        Ok((t, fresh)) => created_or_resent(t, fresh),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn move_thread(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    runner: Option<axum::Extension<RunnerId>>,
    Path(id): Path<i64>,
    Json(m): Json<MoveRequest>,
) -> Response {
    use decurion_proto::ThreadState::*;
    // The runner only reports what happened to a running run, checked in the
    // same transaction as the move. Signing off finished work is the owner's.
    if c == Caller::Runner && m.to == Done {
        return forbidden("marking work done");
    }
    // The owner's tap names the state it was made on, so a stale screen
    // (Stop on work that has since finished) cannot do something else.
    let expect = if c == Caller::Runner {
        Some(Running)
    } else {
        m.expect
    };
    let actor = match runner {
        Some(axum::Extension(r)) if c == Caller::Runner => r.actor(),
        _ => c.actor().to_owned(),
    };
    match st
        .store
        .request_move(
            id,
            expect,
            m.to,
            &actor,
            m.note.as_deref(),
            m.receipt.as_deref(),
        )
        .await
    {
        Ok((t, fresh)) => {
            // Pieces of a goal move on to their check, or settle. A repeated
            // tap changed nothing, so there is nothing to tell the goal.
            let piece = if fresh {
                match st.store.piece_thread_moved(&t).await {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::error!(error = %e, thread = t.id, "could not update the goal");
                        false
                    }
                }
            } else {
                false
            };
            // Only what the agent did is news; the owner's own taps are not. A
            // piece of a goal is not ready for the owner until it is checked.
            let news = c == Caller::Runner && fresh && !(piece && t.state == InReview);
            if let (true, Some(p)) = (news, st.push.clone()) {
                let store = st.store.clone();
                let t = t.clone();
                tokio::spawn(async move {
                    let app = store.app(t.app_id).await.map(|a| a.name);
                    if let Some(n) = push::for_move(&t, app.as_deref().unwrap_or("")) {
                        p.send(&store, &n).await;
                    }
                });
            }
            // The owner accepted it: the hub sends it to GitHub now. The wish
            // was saved with the accept, so a crash here still sends it at
            // the next start.
            if fresh && c == Caller::Owner && t.state == Done {
                if let Some(s) = st.sender() {
                    s.spawn(t.id);
                }
            }
            Json(t).into_response()
        }
        Err(e) => ApiError(e).into_response(),
    }
}

#[derive(Deserialize)]
struct After {
    #[serde(default)]
    after: i64,
    /// Instead of paging from the start, return the last `tail` events.
    tail: Option<i64>,
}

async fn list_events(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<After>,
) -> ApiResult<Vec<decurion_proto::EventRecord>> {
    match q.tail {
        Some(n) => Ok(Json(st.store.events_tail(id, n.clamp(1, 500)).await?)),
        None => Ok(Json(st.store.events(id, q.after).await?)),
    }
}

async fn add_event(
    State(st): State<AppState>,
    runner: Option<axum::Extension<RunnerId>>,
    Path(id): Path<i64>,
    Json(e): Json<RunEvent>,
) -> Response {
    let Some(axum::Extension(r)) = runner else {
        return (StatusCode::FORBIDDEN, "events come from the runner").into_response();
    };
    // The owner's messages and cut-off steps are the hub's record; a runner
    // writing them could put words in the owner's mouth.
    if e.hub_only() {
        return (
            StatusCode::FORBIDDEN,
            "only the hub writes this kind of event",
        )
            .into_response();
    }
    // Plan usage readings feed the meters, not the activity log.
    if matches!(e, RunEvent::Usage { .. }) {
        return match st.store.record_usage(id, &r.name, &e).await {
            Ok(_) => StatusCode::NO_CONTENT.into_response(),
            Err(e) => ApiError(e).into_response(),
        };
    }
    match st.store.add_event(id, &r.name, &e).await {
        Ok((eid, new_outage)) => {
            if let (true, Some(p)) = (new_outage, st.push.clone()) {
                let store = st.store.clone();
                tokio::spawn(async move {
                    // One alert per account, with the work now waiting on it.
                    if let Ok(Some(l)) = store.limit_for_thread(id).await {
                        let app = match store.thread(id).await {
                            Ok(t) => store
                                .app(t.app_id)
                                .await
                                .map(|a| a.name)
                                .unwrap_or_default(),
                            Err(_) => String::new(),
                        };
                        let head = format!("Claude Max {} is out of usage", l.profile.as_str());
                        let body = format!(
                            "{} waiting; they restart on their own when it resets. Open to continue on pay-per-use.",
                            if l.waiting == 1 { "1 job".to_owned() } else { format!("{} jobs", l.waiting) }
                        );
                        p.send(&store, &push::Notice::new(&app, &head, &body)).await;
                    }
                });
            }
            (StatusCode::CREATED, Json(eid)).into_response()
        }
        Err(e) => ApiError(e).into_response(),
    }
}

async fn list_messages(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Vec<decurion_proto::Message>> {
    Ok(Json(st.store.messages(id).await?))
}

async fn add_message(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Path(id): Path<i64>,
    Json(m): Json<NewMessage>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("messaging an agent");
    }
    match st.store.add_message(id, &m).await {
        Ok((msg, fresh)) => {
            // A message can send a goal's piece back to its builder.
            if fresh {
                if let Ok(t) = st.store.thread(id).await {
                    if let Err(e) = st.store.piece_thread_moved(&t).await {
                        tracing::error!(error = %e, thread = id, "could not update the goal");
                    }
                }
            }
            created_or_resent(msg, fresh)
        }
        Err(e) => ApiError(e).into_response(),
    }
}

async fn live(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Path(id): Path<i64>,
) -> Response {
    if c != Caller::Runner {
        return not_runner("checking on a run");
    }
    match st.store.live(id).await {
        Ok(mut l) => {
            // Whether the owner is watching this run's browser now.
            l.browser = st.browser.want(id);
            Json(l).into_response()
        }
        Err(e) => ApiError(e).into_response(),
    }
}

#[derive(Deserialize)]
struct Ids {
    ids: Vec<i64>,
}

async fn messages_delivered(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Path(id): Path<i64>,
    Json(b): Json<Ids>,
) -> Response {
    if c != Caller::Runner {
        return not_runner("marking messages delivered");
    }
    match st.store.mark_delivered(id, &b.ids).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

#[derive(Deserialize)]
struct RunnerName {
    runner: String,
    /// Checked again at every claim, so a runner left on old rules after the
    /// hub updates never starts new work.
    #[serde(default)]
    rules_version: Option<u32>,
}

/// The 409 a runner on other rules gets, telling it which rules and commit
/// to update to.
fn other_rules(r: &RunnerId, theirs: Option<u32>) -> Response {
    tracing::warn!(runner = %r.name, ?theirs, "runner on other rules refused");
    let reply = HelloReply {
        paused: vec![],
        rules_version: RULES_VERSION,
        hub_commit: HUB_COMMIT.to_owned(),
    };
    (StatusCode::CONFLICT, Json(reply)).into_response()
}

/// The runner making a runner-only call. A runner names itself in some
/// bodies; that name must be the one its key belongs to.
// Responses are what axum handlers return; boxing them buys nothing here.
#[allow(clippy::result_large_err)]
fn runner_only(
    runner: Option<axum::Extension<RunnerId>>,
    claimed: Option<&str>,
) -> Result<RunnerId, Response> {
    match runner {
        Some(axum::Extension(r)) if claimed.is_none_or(|n| n == r.name) => Ok(r),
        Some(_) => Err((
            StatusCode::FORBIDDEN,
            "that runner name belongs to another key",
        )
            .into_response()),
        None => Err(forbidden("runner calls are runner-only; this")),
    }
}

async fn runner_hello(
    State(st): State<AppState>,
    runner: Option<axum::Extension<RunnerId>>,
    Json(h): Json<RunnerHello>,
) -> Response {
    let r = match runner_only(runner, Some(&h.runner)) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    // Work it had running goes back to the queue either way, so a runner
    // that must update first does not leave work stuck as Running.
    if let Err(e) = st.store.pause_runner_helpers(&r.name).await {
        return ApiError(e).into_response();
    }
    let paused = match st.store.pause_runner_threads(&r.name).await {
        Ok(ids) => ids,
        Err(e) => return ApiError(e).into_response(),
    };
    // A runner on other rules must not touch any work; it updates itself to
    // the hub's commit and starts again.
    if h.rules_version != RULES_VERSION {
        return other_rules(&r, Some(h.rules_version));
    }
    Json(HelloReply {
        paused,
        rules_version: RULES_VERSION,
        hub_commit: HUB_COMMIT.to_owned(),
    })
    .into_response()
}

async fn runner_claim(
    State(st): State<AppState>,
    runner: Option<axum::Extension<RunnerId>>,
    Json(b): Json<RunnerName>,
) -> Response {
    let r = match runner_only(runner, Some(&b.runner)) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if b.rules_version != Some(RULES_VERSION) {
        return other_rules(&r, b.rules_version);
    }
    match st.store.claim(&r.name, r.place).await {
        Ok(out) => {
            if let (Some(p), Some(first)) = (st.push.clone(), out.capped.first()) {
                // One alert, however many pieces of work it stopped.
                let body = match out.capped.len() {
                    1 => first.title.clone(),
                    n => format!("{n} pieces of work are waiting"),
                };
                let mut apps: Vec<i64> = out.capped.iter().map(|t| t.app_id).collect();
                apps.dedup();
                let store = st.store.clone();
                tokio::spawn(async move {
                    let mut names = Vec::new();
                    for a in apps {
                        if let Ok(a) = store.app(a).await {
                            if !names.contains(&a.name) {
                                names.push(a.name);
                            }
                        }
                    }
                    let n = push::Notice::new(&names.join(", "), "spending cap reached", &body);
                    p.send(&store, &n).await;
                });
            }
            match out.work {
                Some(claim) => Json(claim).into_response(),
                None => StatusCode::NO_CONTENT.into_response(),
            }
        }
        Err(e) => ApiError(e).into_response(),
    }
}

async fn service_worker() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        include_str!("sw.js"),
    )
        .into_response()
}

/// The board's and inbox's shared stylesheet, script and icons. `no-cache`
/// so a new hub build is picked up on the next load.
fn asset(content_type: &'static str, body: impl IntoResponse) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body,
    )
        .into_response()
}

async fn manifest() -> Response {
    (
        [(header::CONTENT_TYPE, "application/manifest+json")],
        include_str!("manifest.webmanifest"),
    )
        .into_response()
}

async fn push_key(State(st): State<AppState>) -> Response {
    match &st.push {
        Some(p) => Json(serde_json::json!({ "key": p.public_key() })).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            "notifications are not set up on this hub",
        )
            .into_response(),
    }
}

async fn push_subscribe(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    axum::Extension(DeviceId(device)): axum::Extension<DeviceId>,
    Json(sub): Json<devices::PushSubscription>,
) -> Response {
    let Some(device) = device.filter(|_| c == Caller::Owner) else {
        return (
            StatusCode::FORBIDDEN,
            "subscribe from a signed-in phone or browser",
        )
            .into_response();
    };
    match st.store.save_push(device, &sub).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn get_spend(State(st): State<AppState>) -> ApiResult<decurion_proto::Spend> {
    Ok(Json(st.store.spend().await?))
}

#[derive(Deserialize)]
struct Cap {
    cap_usd: f64,
    /// Which paid account; the Anthropic key if left out.
    #[serde(default)]
    account: Option<decurion_proto::Account>,
}

async fn set_cap(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Json(b): Json<Cap>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("changing the spending cap");
    }
    let account = b.account.unwrap_or(decurion_proto::Account::Api);
    match st.store.set_cap(account, b.cap_usd).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

#[derive(Deserialize)]
struct OnOff {
    on: bool,
}

async fn set_pay_per_use(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Path(id): Path<i64>,
    Json(b): Json<OnOff>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("choosing how an app is billed");
    }
    match st.store.set_pay_per_use(id, b.on).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn list_accounts(State(st): State<AppState>) -> ApiResult<Vec<decurion_proto::AccountMeter>> {
    Ok(Json(st.store.accounts().await?))
}

#[derive(Deserialize)]
struct AppCap {
    /// `null` removes the app's own cap.
    cap_usd: Option<f64>,
}

async fn set_app_cap(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Path(id): Path<i64>,
    Json(b): Json<AppCap>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("changing an app's spending cap");
    }
    match st.store.set_app_cap(id, b.cap_usd).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn cloud_page() -> Response {
    page(include_str!("cloud.html"))
}

async fn set_place(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Path(id): Path<i64>,
    Json(p): Json<PlaceRequest>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("moving work between computers");
    }
    match st.store.set_place(id, p.place, "owner").await {
        Ok(t) => Json(t).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn list_workers(State(st): State<AppState>) -> ApiResult<Vec<workers::Worker>> {
    let worker = |id, name: String, place| {
        let ago = st.seconds_ago(&name);
        workers::Worker {
            id,
            online: ago.is_some_and(|s| s <= ONLINE_SECS),
            seconds_ago: ago,
            name,
            place,
        }
    };
    let mut list = vec![worker(None, workers::LOCAL.to_owned(), Place::Pc)];
    for (id, name) in st.store.cloud_workers().await? {
        list.push(worker(Some(id), name, Place::Cloud));
    }
    Ok(Json(list))
}

async fn new_join_code(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("adding a cloud worker");
    }
    match st.store.new_join_code().await {
        Ok((code, name)) => Json(serde_json::json!({
            "code": code,
            "name": name,
            "minutes": workers::JOIN_HOURS * 60,
            "hub_url": st.public_url.as_deref(),
            "hub_commit": HUB_COMMIT,
            "rules_version": RULES_VERSION,
            "repo": REPO_URL,
        }))
        .into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn remove_worker(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Path(id): Path<i64>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("removing a cloud worker");
    }
    match st.store.remove_cloud_worker(id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// A cloud worker's first call: its setup code for its own key.
async fn runner_join(State(st): State<AppState>, Json(j): Json<JoinRequest>) -> Response {
    match st.store.join_cloud_worker(&j.code).await {
        Ok(Some((name, token))) => {
            tracing::info!(%name, "cloud worker signed up");
            Json(JoinReply { name, token }).into_response()
        }
        Ok(None) => (
            StatusCode::UNAUTHORIZED,
            "This setup code is used or expired. Make new setup text in Decurion.",
        )
            .into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// Whether a runner call reads a work's saved copies or changes them.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Access {
    Read,
    Write,
}

/// Checks the caller is a runner holding `thread` (or the PC runner, for a
/// read), and that the hub can keep files. Returns the runner and the file
/// store.
// Responses are what axum handlers return; boxing them buys nothing here.
#[allow(clippy::result_large_err)]
async fn holding(
    st: &AppState,
    runner: Option<axum::Extension<RunnerId>>,
    thread: i64,
    access: Access,
) -> Result<(RunnerId, Arc<Files>), Response> {
    let r = runner_only(runner, None)?;
    let files = st.files.clone().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "this hub has no folder for saved work",
        )
            .into_response()
    })?;
    // The PC runner checks finished work, whichever computer made it, so it
    // may read any work's saved copies. It runs next to the hub, which keeps
    // the owner's repos anyway.
    if access == Access::Read && r.place == Place::Pc {
        return Ok((r, files));
    }
    match st.store.running_on(thread, &r.name).await {
        Ok(true) => Ok((r, files)),
        Ok(false) => Err((
            StatusCode::CONFLICT,
            "that work is not running on this runner",
        )
            .into_response()),
        Err(e) => Err(ApiError(e).into_response()),
    }
}

async fn runner_base(
    State(st): State<AppState>,
    runner: Option<axum::Extension<RunnerId>>,
    Path(id): Path<i64>,
) -> Response {
    let (_, files) = match holding(&st, runner, id, Access::Read).await {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    let result = async {
        let t = st.store.thread(id).await?;
        let app = st.store.app(t.app_id).await?;
        let base = st.store.base_commit(id).await?;
        let (sha, path) = files
            .base_bundle(app.id, &app.repo_path, base.as_deref())
            .await?;
        let sha = st.store.fix_base_commit(id, &sha).await?;
        Ok::<_, StoreError>((sha, path))
    }
    .await;
    match result {
        Ok((sha, path)) => file_response(&path, Some(("x-decurion-base", sha))).await,
        Err(e) => ApiError(e).into_response(),
    }
}

async fn runner_upload(
    State(st): State<AppState>,
    runner: Option<axum::Extension<RunnerId>>,
    Path((id, part)): Path<(i64, String)>,
    body: Body,
) -> Response {
    let Some(part) = Part::parse(&part) else {
        return (StatusCode::NOT_FOUND, "unknown part").into_response();
    };
    let (_, files) = match holding(&st, runner, id, Access::Write).await {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    if let Err(e) = files.prepare_thread_dir(id) {
        tracing::error!(%e, "could not make the saved-work folder");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    match save_body(body, &files.incoming(id, part), part.max_bytes()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(resp) => resp,
    }
}

async fn runner_checkpoint(
    State(st): State<AppState>,
    runner: Option<axum::Extension<RunnerId>>,
    Path(id): Path<i64>,
    Json(c): Json<NewCheckpoint>,
) -> Response {
    let (r, files) = match holding(&st, runner, id, Access::Write).await {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    match st
        .store
        .add_checkpoint(&files, id, &r.name, &c.head, c.base.as_deref())
        .await
    {
        Ok(cp) => Json(serde_json::json!({ "id": cp })).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn runner_download(
    State(st): State<AppState>,
    runner: Option<axum::Extension<RunnerId>>,
    Path((id, cp, part)): Path<(i64, i64, String)>,
) -> Response {
    // Only the runner now running the work may read its saved copies.
    let (_, files) = match holding(&st, runner, id, Access::Read).await {
        Ok(h) => h,
        Err(resp) => return resp,
    };
    let Some(part) = Part::parse(&part) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match st.store.checkpoint_belongs(id, cp).await {
        Ok(true) => file_response(&files.saved(id, cp, part), None).await,
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn runner_credential(
    State(st): State<AppState>,
    runner: Option<axum::Extension<RunnerId>>,
    Path(id): Path<i64>,
) -> Response {
    let r = match runner_only(runner, None) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match st.store.running_on(id, &r.name).await {
        Ok(true) => {}
        Ok(false) => {
            return (
                StatusCode::CONFLICT,
                "that work is not running on this runner",
            )
                .into_response()
        }
        Err(e) => return ApiError(e).into_response(),
    }
    let account = match st.store.open_run_account(id).await {
        Ok(Some(a)) => a,
        Ok(None) => return (StatusCode::CONFLICT, "that work has no open run").into_response(),
        Err(e) => return ApiError(e).into_response(),
    };
    match secrets::read(&account) {
        Some(c) => Json(c).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            format!(
                "no sign-in or key saved on the PC for {}",
                account
                    .parse::<decurion_proto::Account>()
                    .map_or(account.clone(), |a| a.label().to_owned())
            ),
        )
            .into_response(),
    }
}

/// Streams a file to the caller, with an extra header if given.
async fn file_response(path: &std::path::Path, extra: Option<(&'static str, String)>) -> Response {
    let Ok(file) = tokio::fs::File::open(path).await else {
        return (StatusCode::NOT_FOUND, "that saved work is gone").into_response();
    };
    stream_file(file, extra, None).await
}

/// Removes a file when dropped. A temporary file being streamed goes once
/// the stream ends or the caller hangs up, whichever comes first.
pub(crate) struct RemoveOnDrop(pub(crate) std::path::PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.0) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(error = %e, path = %self.0.display(), "could not remove a temporary file");
            }
        }
    }
}

/// Streams an open file as `application/octet-stream`, with an extra header
/// if given, then removes it if asked to.
pub(crate) async fn stream_file(
    file: tokio::fs::File,
    extra: Option<(&'static str, String)>,
    then_remove: Option<RemoveOnDrop>,
) -> Response {
    use tokio::io::AsyncReadExt;
    let len = file.metadata().await.map(|m| m.len()).ok();
    // The file comes first so it is closed before the guard removes it.
    let stream =
        futures_util::stream::try_unfold((file, then_remove), |(mut f, gone)| async move {
            let mut buf = vec![0u8; 64 << 10];
            let n = f.read(&mut buf).await?;
            if n == 0 {
                return Ok::<_, std::io::Error>(None);
            }
            buf.truncate(n);
            Ok(Some((Bytes::from(buf), (f, gone))))
        });
    let mut resp = Body::from_stream(stream).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/octet-stream"),
    );
    if let Some(n) = len {
        h.insert(header::CONTENT_LENGTH, n.into());
    }
    if let Some((k, v)) = extra {
        if let Ok(v) = header::HeaderValue::from_str(&v) {
            h.insert(k, v);
        }
    }
    resp
}

/// Writes an upload to `path`, refusing anything over `max` bytes. The file
/// only appears under its real name once complete.
// Responses are what axum handlers return; boxing them buys nothing here.
#[allow(clippy::result_large_err)]
async fn save_body(body: Body, path: &std::path::Path, max: u64) -> Result<(), Response> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    let tmp = path.with_extension("partial");
    let fail = |code: StatusCode, msg: &'static str| (code, msg).into_response();
    let mut f = tokio::fs::File::create(&tmp)
        .await
        .map_err(|_| fail(StatusCode::INTERNAL_SERVER_ERROR, "could not save upload"))?;
    let mut stream = body.into_data_stream();
    let mut total: u64 = 0;
    let result = async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| fail(StatusCode::BAD_REQUEST, "upload broke off"))?;
            total += chunk.len() as u64;
            if total > max {
                return Err(fail(StatusCode::PAYLOAD_TOO_LARGE, "upload too large"));
            }
            f.write_all(&chunk)
                .await
                .map_err(|_| fail(StatusCode::INTERNAL_SERVER_ERROR, "could not save upload"))?;
        }
        f.flush()
            .await
            .map_err(|_| fail(StatusCode::INTERNAL_SERVER_ERROR, "could not save upload"))?;
        Ok(())
    }
    .await;
    drop(f);
    match result {
        Ok(()) => tokio::fs::rename(&tmp, path)
            .await
            .map_err(|_| fail(StatusCode::INTERNAL_SERVER_ERROR, "could not save upload")),
        Err(resp) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(resp)
        }
    }
}

async fn get_limits(State(st): State<AppState>) -> ApiResult<Vec<decurion_proto::Limit>> {
    Ok(Json(st.store.limits().await?))
}

/// The one tap that moves work waiting on an exhausted Max account to
/// pay-per-use (still under the monthly cap).
async fn overflow(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Path(profile): Path<String>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("moving work to pay-per-use");
    }
    let Ok(p) = profile.parse::<decurion_proto::Profile>() else {
        return (StatusCode::BAD_REQUEST, "unknown account").into_response();
    };
    match st.store.overflow_to_pay_per_use(p).await {
        Ok(n) => Json(serde_json::json!({ "moved": n })).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}
/// The board's "save a test key" form. Its value is never logged.
#[derive(Deserialize)]
struct NewTestKey {
    name: String,
    value: String,
}

impl std::fmt::Debug for NewTestKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewTestKey")
            .field("name", &self.name)
            .field("value", &"<hidden>")
            .finish()
    }
}

fn no_vault() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the key store is not set up on this hub",
    )
        .into_response()
}

async fn list_test_keys(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Path(id): Path<i64>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("seeing test keys");
    }
    if st.vault.is_none() {
        return no_vault();
    }
    match st.store.test_keys(id).await {
        Ok(k) => Json(k).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// Saves a test key: the value goes to the vault, and only its name and last
/// four characters to the database. The body is parsed here rather than by
/// an extractor, so no error message can ever repeat the value back.
async fn add_test_key(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Response {
    if c != Caller::Owner {
        return forbidden("adding test keys");
    }
    let Some(vault) = st.vault.clone() else {
        return no_vault();
    };
    let Ok(k) = serde_json::from_slice::<NewTestKey>(&body) else {
        return (StatusCode::BAD_REQUEST, "send a name and a value").into_response();
    };
    let checked =
        decurion_vault::check_name(&k.name).and_then(|()| decurion_vault::check_value(&k.value));
    if let Err(e) = checked {
        return vault_error(e, id, &k.name);
    }
    if let Err(e) = st.store.app(id).await {
        return ApiError(e).into_response();
    }
    // The vault first: a crash before the row is written leaves at worst an
    // unlisted value, never a listed key with nothing behind it.
    let last_four = decurion_vault::last_four(&k.value);
    let NewTestKey { name, value } = k;
    let task_name = name.clone();
    let saved = tokio::task::spawn_blocking(move || vault.put(id, &task_name, &value)).await;
    match saved {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return vault_error(e, id, &name),
        Err(e) => {
            tracing::error!(error = %e, app = id, name = %name, "key store task failed");
            return key_store_failed();
        }
    }
    match st.store.save_test_key(id, &name, &last_four).await {
        Ok(t) => (StatusCode::CREATED, Json(t)).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// Forgets a test key: the value first, then the row, so a failed removal
/// still shows on the board and can be retried. A crash in between leaves a
/// listed key with no value, which the runner refuses to start with.
async fn remove_test_key(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Path((id, name)): Path<(i64, String)>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("removing test keys");
    }
    let Some(vault) = st.vault.clone() else {
        return no_vault();
    };
    // Also keeps odd names out of the vault's file and entry names.
    if let Err(e) = decurion_vault::check_name(&name) {
        return vault_error(e, id, &name);
    }
    if let Err(e) = st.store.app(id).await {
        return ApiError(e).into_response();
    }
    let task_name = name.clone();
    match tokio::task::spawn_blocking(move || vault.delete(id, &task_name)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return vault_error(e, id, &name),
        Err(e) => {
            tracing::error!(error = %e, app = id, name = %name, "key store task failed");
            return key_store_failed();
        }
    }
    match st.store.remove_test_key(id, &name).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// A refused name or value says why; a key store failure says only that it
/// failed. The value is never part of either.
fn vault_error(e: decurion_vault::VaultError, app: i64, name: &str) -> Response {
    match e {
        decurion_vault::VaultError::Invalid(why) => (StatusCode::BAD_REQUEST, why).into_response(),
        decurion_vault::VaultError::Store(why) => {
            tracing::error!(error = %why, app, name, "key store error");
            key_store_failed()
        }
    }
}

fn key_store_failed() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "could not use the key store on this PC",
    )
        .into_response()
}

#[derive(Deserialize)]
struct Sites {
    sites: Vec<String>,
}

async fn set_sites(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Path(id): Path<i64>,
    Json(b): Json<Sites>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("choosing which websites an app's agents may use");
    }
    match st.store.set_sites(id, &b.sites).await {
        Ok(sites) => Json(sites).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// Sites every box may reach whatever the app, so the board can list them.
async fn built_in_sites() -> Json<&'static [&'static str]> {
    Json(decurion_proto::sites::BUILT_IN)
}
