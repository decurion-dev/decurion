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
pub mod limits;
pub mod push;
pub mod spend;
pub mod store;
pub mod usage;

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
    push: Option<push::Pusher>,
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
        }
    }

    /// Turns on phone notifications.
    pub fn with_push(mut self, p: push::Pusher) -> Self {
        self.push = Some(p);
        self
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
        .route("/apps/{id}/cap", post(set_app_cap))
        .route("/accounts", get(list_accounts))
        .route("/push/subscribe", post(push_subscribe))
        .layer(middleware::from_fn_with_state(state.clone(), require_key));
    Router::new()
        .route("/", get(board))
        .route("/inbox", get(inbox))
        .route("/login", get(login_page).post(login))
        .route("/pair", get(pair_page).post(pair))
        .route("/sw.js", get(service_worker))
        .route("/manifest.webmanifest", get(manifest))
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
        (Some(k), _) if same(k, &st.keys.runner) => (Caller::Runner, None),
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
        Ok(t) => {
            // Only what the agent did is news; the owner's own taps are not.
            let news = (c == Caller::Runner)
                .then(|| push::message_for(t.state, &t.title))
                .flatten();
            if let (Some(p), Some((title, body))) = (st.push.clone(), news) {
                let store = st.store.clone();
                tokio::spawn(async move { p.notify(&store, &title, &body).await });
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
    axum::Extension(c): axum::Extension<Caller>,
    Path(id): Path<i64>,
    Json(e): Json<RunEvent>,
) -> Response {
    if c != Caller::Runner {
        return (StatusCode::FORBIDDEN, "events come from the runner").into_response();
    }
    // Plan usage readings feed the meters, not the activity log.
    if matches!(e, RunEvent::Usage { .. }) {
        return match st.store.record_usage(id, &e).await {
            Ok(_) => StatusCode::NO_CONTENT.into_response(),
            Err(e) => ApiError(e).into_response(),
        };
    }
    match st.store.add_event(id, &e).await {
        Ok((eid, new_outage)) => {
            if let (true, Some(p)) = (new_outage, st.push.clone()) {
                let store = st.store.clone();
                tokio::spawn(async move {
                    // One alert per account, with the work now waiting on it.
                    if let Ok(Some(l)) = store.limit_for_thread(id).await {
                        let title = format!("Claude Max {} is out of usage", l.profile.as_str());
                        let body = format!(
                            "{} waiting; they restart on their own when it resets. Open to continue on pay-per-use.",
                            if l.waiting == 1 { "1 job".to_owned() } else { format!("{} jobs", l.waiting) }
                        );
                        p.notify(&store, &title, &body).await;
                    }
                });
            }
            (StatusCode::CREATED, Json(eid)).into_response()
        }
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
        Ok(out) => {
            if let (Some(p), Some(first)) = (st.push.clone(), out.capped.first()) {
                // One alert, however many pieces of work it stopped.
                let body = match out.capped.len() {
                    1 => first.title.chars().take(80).collect::<String>(),
                    n => format!("{n} pieces of work are waiting"),
                };
                let store = st.store.clone();
                tokio::spawn(async move { p.notify(&store, "Spending cap reached", &body).await });
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
