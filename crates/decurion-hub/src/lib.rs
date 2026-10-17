//! The Decurion hub: holds board state and serves the owner's board and the
//! runner API.
//!
//! Every `/api` call needs a key. Agents run in containers that can reach the
//! host network, so an unauthenticated API would let an agent move its own
//! card to Done. Containers never receive either key.
//!
//! The browser never holds the owner key: the hub prints a login code that is
//! only valid until the hub restarts, and `/login` trades it for a random
//! session id in an HttpOnly cookie.

pub mod store;

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use decurion_proto::{MoveRequest, NewApp, NewThread, RunEvent};
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use store::{Store, StoreError};

/// Who is calling. The owner uses the board; the runner uses its own key.
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
    sessions: Arc<Mutex<HashSet<String>>>,
}

impl AppState {
    /// `login_code` is printed for the owner at start; it opens browser
    /// sessions until the hub restarts.
    pub fn new(store: Store, keys: Keys, login_code: String) -> Self {
        Self {
            store,
            keys,
            login_code: login_code.into(),
            sessions: Arc::default(),
        }
    }

    fn has_session(&self, id: &str) -> bool {
        self.sessions
            .lock()
            .map(|s| s.iter().any(|known| same(known, id)))
            .unwrap_or(false)
    }
}

const COOKIE: &str = "decurion_session";

/// Random 256-bit token.
pub fn new_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/apps", get(list_apps).post(add_app))
        .route("/threads", get(list_threads).post(add_thread))
        .route("/threads/{id}", get(get_thread))
        .route("/threads/{id}/move", post(move_thread))
        .route("/threads/{id}/events", get(list_events).post(add_event))
        .route("/runner/hello", post(runner_hello))
        .route("/runner/claim", post(runner_claim))
        .layer(middleware::from_fn_with_state(state.clone(), require_key));
    Router::new()
        .route("/", get(board))
        .route("/login", get(login))
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

fn session_cookie(headers: &HeaderMap) -> Option<String> {
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
    let caller = match (bearer(req.headers()), session_cookie(req.headers())) {
        (Some(k), _) if same(k, &st.keys.owner) => Caller::Owner,
        (Some(k), _) if same(k, &st.keys.runner) => Caller::Runner,
        (None, Some(s)) if st.has_session(&s) => Caller::Owner,
        _ => return (StatusCode::UNAUTHORIZED, "missing or wrong key").into_response(),
    };
    req.extensions_mut().insert(caller);
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

async fn board() -> Response {
    (
        [
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'",
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Html(include_str!("board.html")),
    )
        .into_response()
}

#[derive(Deserialize)]
struct LoginQuery {
    code: String,
}

/// The link printed by the hub at start. Opens a browser session; the owner
/// key itself never reaches the browser.
async fn login(State(st): State<AppState>, Query(q): Query<LoginQuery>) -> Response {
    if !same(&q.code, &st.login_code) {
        return (StatusCode::UNAUTHORIZED, "wrong or expired login code").into_response();
    }
    let id = new_secret();
    match st.sessions.lock() {
        Ok(mut s) => s.insert(id.clone()),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let cookie = format!("{COOKIE}={id}; HttpOnly; SameSite=Strict; Path=/");
    ([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response()
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
