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

pub mod devices;
pub mod store;

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use decurion_proto::{MoveRequest, NewApp, NewThread, RunEvent};
use serde::Deserialize;
use std::sync::Arc;
use store::{Store, StoreError};

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
            Caller::Owner => "owner",
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
        }
    }
}

const COOKIE: &str = "decurion_device";

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
        .route("/runner/hello", post(runner_hello))
        .route("/runner/claim", post(runner_claim))
        .route("/pairing", post(new_pairing))
        .route("/devices", get(list_devices))
        .route("/devices/{id}/remove", post(remove_device))
        .layer(middleware::from_fn_with_state(state.clone(), require_key));
    Router::new()
        .route("/", get(board))
        .route("/inbox", get(inbox))
        .route("/login", get(login))
        .route("/pair", get(pair))
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

async fn require_key(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let (caller, device) = match (bearer(req.headers()), device_cookie(req.headers())) {
        (Some(k), _) if same(k, &st.keys.owner) => (Caller::Owner, None),
        (Some(k), _) if same(k, &st.keys.runner) => (Caller::Runner, None),
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
            StoreError::BadMove { .. } | StoreError::Conflict | StoreError::Duplicate(_) => {
                StatusCode::CONFLICT
            }
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

fn forbidden(what: &str) -> Response {
    (StatusCode::FORBIDDEN, format!("{what} is owner-only")).into_response()
}

fn page(html: &'static str) -> Response {
    (
        [
            (header::CONTENT_SECURITY_POLICY, CSP),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Html(html),
    )
        .into_response()
}

async fn board() -> Response {
    page(include_str!("board.html"))
}

async fn inbox() -> Response {
    page(include_str!("inbox.html"))
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

/// The link printed by the hub at start. Signs this browser in as a device;
/// the owner key itself never reaches the browser.
async fn login(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CodeQuery>,
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
async fn pair(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CodeQuery>,
) -> Response {
    match st.store.redeem_pairing_code(&q.code).await {
        Ok(true) => {}
        Ok(false) => {
            return (
                StatusCode::UNAUTHORIZED,
                Html("This pairing code is used or expired. Make a new one on the board."),
            )
                .into_response()
        }
        Err(e) => return ApiError(e).into_response(),
    }
    match st.store.add_device(device_name(&headers)).await {
        Ok((_, token)) => sign_in(&headers, &token, "/inbox"),
        Err(e) => ApiError(e).into_response(),
    }
}

#[derive(serde::Serialize)]
struct Pairing {
    url: String,
    qr_svg: String,
    minutes: i64,
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
    Json(Pairing {
        url,
        qr_svg,
        minutes: devices::PAIRING_MINUTES,
    })
    .into_response()
}

async fn list_devices(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("listing devices");
    }
    match st.store.devices().await {
        Ok(d) => Json(d).into_response(),
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
        Ok(t) => (StatusCode::CREATED, Json(t)).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn move_thread(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Path(id): Path<i64>,
    Json(m): Json<MoveRequest>,
) -> Response {
    use decurion_proto::ThreadState::*;
    // The runner only reports what happened to a running run, checked in the
    // same transaction as the move. Signing off finished work is the owner's.
    if c == Caller::Runner && m.to == Done {
        return forbidden("marking work done");
    }
    let expect = (c == Caller::Runner).then_some(Running);
    match st
        .store
        .move_thread(id, expect, m.to, c.actor(), m.note.as_deref())
        .await
    {
        Ok(t) => Json(t).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

#[derive(Deserialize)]
struct After {
    #[serde(default)]
    after: i64,
}

async fn list_events(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<After>,
) -> ApiResult<Vec<decurion_proto::EventRecord>> {
    Ok(Json(st.store.events(id, q.after).await?))
}

async fn add_event(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Path(id): Path<i64>,
    Json(e): Json<RunEvent>,
) -> Response {
    if c != Caller::Runner {
        return (StatusCode::FORBIDDEN, "events come from the runner").into_response();
    }
    match st.store.add_event(id, &e).await {
        Ok(eid) => (StatusCode::CREATED, Json(eid)).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

#[derive(Deserialize)]
struct RunnerName {
    runner: String,
}

async fn runner_hello(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Json(r): Json<RunnerName>,
) -> Response {
    if c != Caller::Runner {
        return forbidden("runner calls are runner-only; this");
    }
    match st.store.pause_runner_threads(&r.runner).await {
        Ok(ids) => Json(ids).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn runner_claim(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    Json(r): Json<RunnerName>,
) -> Response {
    if c != Caller::Runner {
        return forbidden("runner calls are runner-only; this");
    }
    match st.store.claim(&r.runner).await {
        Ok(Some(claim)) => Json(claim).into_response(),
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}
