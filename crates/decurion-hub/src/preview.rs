//! "Run it": the owner tries an app, or one piece of work as last saved, in
//! a live browser on the board or the phone. This owns the `preview` browser
//! sessions (see `browser.rs` for the relay they share with the agent's test
//! browser) and their routes.
//!
//! Rules that must stay true:
//! - Only the owner starts, stops or lists previews. Only the PC runner
//!   claims one and says it is alive; none of these routes is in
//!   `cloud_reach`, so a cloud worker's key never reaches them.
//! - One preview at a time, kept by the database itself
//!   (`browser_sessions_one_preview`): Run it on something else stops the
//!   one before; a second tap on the same thing reuses it.
//! - A piece of work runs only from a saved copy the hub holds, on its own
//!   branch; research changes no code, so it has nothing to run.
//! - A preview ends when the owner taps Stop, when nobody has had it open
//!   for [`PREVIEW_IDLE_SECS`](decurion_proto::preview::PREVIEW_IDLE_SECS),
//!   when its runner stops saying it is alive for
//!   [`PREVIEW_STALE_SECS`], or when no runner picks it up within
//!   [`PREVIEW_UNCLAIMED_SECS`]. The hub never sends the app's test keys,
//!   only their names.

use crate::browser::{end_in, session_from, session_in, BrowserSession, COLS, NOW};
use crate::store::{app_from, Result, Store, StoreError, APP_COLS};
use crate::workers::RunnerId;
use crate::{forbidden, runner_only, ApiError, AppState, Caller};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use decurion_proto::preview::{
    PreviewClaim, PreviewEnded, PreviewState, PreviewThread, PREVIEW_STALE_SECS,
    PREVIEW_UNCLAIMED_SECS,
};
use decurion_proto::Place;
use sqlx::{Row, Sqlite, Transaction};
use std::time::Duration;

/// What the screen of a preview stopped for another one is told.
pub const REPLACED: &str = "Stopped so another preview could run. Only one runs at a time.";
/// What the screen is told when the owner taps Stop.
pub const STOPPED: &str = "Stopped.";
/// What the screen is told when nobody had the preview open for a while.
pub const IDLE: &str = "Stopped because nobody had it open for 15 minutes.";
const STALE: &str = "Your PC stopped running this preview. Tap Run it to try again.";
const UNCLAIMED: &str =
    "Your PC did not start this preview. Check Decurion is running on your PC, then tap Run it again.";
const LOST: &str =
    "This work has no saved copy to run any more. Run it once the agent has finished a run.";
const RESEARCH: &str = "Research changes no code, so there is nothing new to run.";
const NOTHING_SAVED: &str =
    "Nothing has been saved for this work yet. Run it once the agent has finished a run.";
const NO_BRANCH: &str =
    "This work has no branch of its own yet. Run it once the agent has finished a run.";
/// The most of a runner's reason the owner's screen is shown.
const MAX_WHY: usize = 300;

/// The routes, merged into the `/api` router behind the same keys.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/threads/{id}/preview", post(start_thread))
        .route("/apps/{id}/preview", post(start_app))
        .route("/browser/{sid}/stop", post(stop))
        .route("/previews", get(list))
        // Not in cloud_reach: previews run on the PC only.
        .route("/runner/preview/claim", post(runner_claim))
        .route("/runner/preview/{sid}/alive", post(runner_alive))
        .route("/runner/preview/{sid}/end", post(runner_end))
}

impl AppState {
    /// How long a preview may go with nobody having it open before it is
    /// stopped. Normally
    /// [`PREVIEW_IDLE_SECS`](decurion_proto::preview::PREVIEW_IDLE_SECS).
    pub fn with_preview_idle(mut self, d: Duration) -> Self {
        self.browser.preview_idle = d;
        self
    }
}

// ---------- What the database keeps ----------

/// What the owner tapped Run it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewTarget {
    /// The app as it is in its own folder on the PC.
    App(i64),
    /// A piece of work, as last saved.
    Thread(i64),
}

/// A preview that ended without anyone tapping Stop, and what its screen is
/// told.
pub type Ended = (i64, &'static str);

/// What Run it got: the preview, the ones it stopped to make room, and any
/// found gone stale on the way.
#[derive(Debug, Clone)]
pub struct Started {
    pub session: BrowserSession,
    pub replaced: Vec<i64>,
    pub stale: Vec<Ended>,
}

/// Ends previews whose runner went quiet, and ones no runner picked up.
async fn close_stale(tx: &mut Transaction<'_, Sqlite>) -> Result<Vec<Ended>> {
    let gone: Vec<i64> = sqlx::query_scalar(&format!(
        "UPDATE browser_sessions SET state = 'failed', controller = 'agent',
             updated_at = {NOW}, ended_at = {NOW}
         WHERE kind = 'preview' AND state IN ('pending', 'live') AND runner IS NOT NULL
           AND COALESCE(seen_at, updated_at)
               < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-{PREVIEW_STALE_SECS} seconds')
         RETURNING id"
    ))
    .fetch_all(&mut **tx)
    .await?;
    let unclaimed: Vec<i64> = sqlx::query_scalar(&format!(
        "UPDATE browser_sessions SET state = 'closed', controller = 'agent',
             updated_at = {NOW}, ended_at = {NOW}
         WHERE kind = 'preview' AND state = 'pending' AND runner IS NULL
           AND created_at
               < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-{PREVIEW_UNCLAIMED_SECS} seconds')
         RETURNING id"
    ))
    .fetch_all(&mut **tx)
    .await?;
    Ok(gone
        .into_iter()
        .map(|id| (id, STALE))
        .chain(unclaimed.into_iter().map(|id| (id, UNCLAIMED)))
        .collect())
}

/// The latest saved copy of a piece of work and its branch, if it can be
/// run.
async fn runnable_in(
    tx: &mut Transaction<'_, Sqlite>,
    thread: i64,
) -> Result<std::result::Result<PreviewThread, &'static str>> {
    let row = sqlx::query(
        "SELECT t.title, t.branch, t.research,
                (SELECT MAX(c.id) FROM checkpoints c WHERE c.thread_id = t.id) AS checkpoint
         FROM threads t WHERE t.id = ?",
    )
    .bind(thread)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(StoreError::NotFound)?;
    if row.try_get::<bool, _>("research")? {
        return Ok(Err(RESEARCH));
    }
    let Some(checkpoint) = row.try_get::<Option<i64>, _>("checkpoint")? else {
        return Ok(Err(NOTHING_SAVED));
    };
    let Some(branch) = row.try_get::<Option<String>, _>("branch")? else {
        return Ok(Err(NO_BRANCH));
    };
    Ok(Ok(PreviewThread {
        thread_id: thread,
        title: row.try_get("title")?,
        branch,
        checkpoint,
    }))
}

impl Store {
    /// Runs an app, or a piece of work as last saved, for the owner: the
    /// open preview of the same thing if there is one, otherwise a new one
    /// for the PC runner to pick up, stopping any other.
    pub async fn start_preview(&self, target: PreviewTarget) -> Result<Started> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let stale = close_stale(&mut tx).await?;
        let (app_id, thread_id) = match target {
            PreviewTarget::App(app) => {
                let found: Option<i64> = sqlx::query_scalar("SELECT id FROM apps WHERE id = ?")
                    .bind(app)
                    .fetch_optional(&mut *tx)
                    .await?;
                (found.ok_or(StoreError::NotFound)?, None)
            }
            PreviewTarget::Thread(thread) => {
                runnable_in(&mut tx, thread)
                    .await?
                    .map_err(|why| StoreError::Refused(why.into()))?;
                let app: i64 = sqlx::query_scalar("SELECT app_id FROM threads WHERE id = ?")
                    .bind(thread)
                    .fetch_one(&mut *tx)
                    .await?;
                (app, Some(thread))
            }
        };
        let open = sqlx::query(&format!(
            "SELECT {COLS} FROM browser_sessions
             WHERE kind = 'preview' AND state IN ('pending', 'live') ORDER BY id"
        ))
        .fetch_all(&mut *tx)
        .await?;
        let mut replaced = Vec::new();
        for row in &open {
            let s = session_from(row)?;
            if s.app_id == Some(app_id) && s.thread_id == thread_id {
                tx.commit().await?;
                return Ok(Started {
                    session: s,
                    replaced,
                    stale,
                });
            }
        }
        for row in &open {
            let s = session_from(row)?;
            end_in(&mut tx, &s, "closed").await?;
            replaced.push(s.id);
        }
        // The owner drives a preview from the start; no agent is in it.
        let row = sqlx::query(&format!(
            "INSERT INTO browser_sessions (kind, app_id, thread_id, controller, state, created_at)
             VALUES ('preview', ?, ?, 'owner', 'pending', {NOW}) RETURNING {COLS}"
        ))
        .bind(app_id)
        .bind(thread_id)
        .fetch_one(&mut *tx)
        .await?;
        let session = session_from(&row)?;
        tx.commit().await?;
        Ok(Started {
            session,
            replaced,
            stale,
        })
    }

    /// The PC runner takes the oldest preview waiting for one.
    pub async fn claim_preview(&self, runner: &RunnerId) -> Result<Option<PreviewClaim>> {
        Ok(self.claim_preview_ending(runner).await?.0)
    }

    /// [`Store::claim_preview`], and the previews it ended on the way, for
    /// their screens to be told.
    pub(crate) async fn claim_preview_ending(
        &self,
        runner: &RunnerId,
    ) -> Result<(Option<PreviewClaim>, Vec<Ended>)> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let mut ended = close_stale(&mut tx).await?;
        loop {
            let row = sqlx::query(&format!(
                "SELECT {COLS} FROM browser_sessions
                 WHERE kind = 'preview' AND state = 'pending' AND runner IS NULL
                 ORDER BY id LIMIT 1"
            ))
            .fetch_optional(&mut *tx)
            .await?;
            let Some(row) = row else {
                tx.commit().await?;
                return Ok((None, ended));
            };
            let s = session_from(&row)?;
            let app_id = s.app_id.ok_or(StoreError::NotFound)?;
            let thread = match s.thread_id {
                None => None,
                Some(t) => match runnable_in(&mut tx, t).await {
                    Ok(Ok(p)) => Some(p),
                    // Its saved copy or branch went away since the tap.
                    Ok(Err(_)) | Err(StoreError::NotFound) => {
                        end_in(&mut tx, &s, "failed").await?;
                        ended.push((s.id, LOST));
                        continue;
                    }
                    Err(e) => return Err(e),
                },
            };
            let app = sqlx::query(&format!("SELECT {APP_COLS} FROM apps WHERE id = ?"))
                .bind(app_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(StoreError::NotFound)?;
            let app = app_from(&app)?;
            let test_keys: Vec<String> =
                sqlx::query_scalar("SELECT name FROM test_keys WHERE app_id = ? ORDER BY name")
                    .bind(app_id)
                    .fetch_all(&mut *tx)
                    .await?;
            let done = sqlx::query(&format!(
                "UPDATE browser_sessions SET runner = ?, seen_at = {NOW}, updated_at = {NOW}
                 WHERE id = ? AND state = 'pending' AND runner IS NULL"
            ))
            .bind(&runner.name)
            .bind(s.id)
            .execute(&mut *tx)
            .await?;
            if done.rows_affected() != 1 {
                return Err(StoreError::Conflict);
            }
            tx.commit().await?;
            return Ok((
                Some(PreviewClaim {
                    session_id: s.id,
                    app,
                    thread,
                    test_keys,
                }),
                ended,
            ));
        }
    }

    /// The runner says a preview is still running. False if it should stop:
    /// it ended, is not a preview, or is not this runner's.
    pub async fn preview_alive(&self, sid: i64, runner: &RunnerId) -> Result<bool> {
        let done = sqlx::query(&format!(
            "UPDATE browser_sessions SET seen_at = {NOW}
             WHERE id = ? AND kind = 'preview' AND runner = ? AND state IN ('pending', 'live')"
        ))
        .bind(sid)
        .bind(&runner.name)
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() == 1)
    }

    /// The runner holding a preview ends it. One already ended is left as
    /// it is.
    pub async fn end_preview_by_runner(
        &self,
        sid: i64,
        runner: &RunnerId,
        failed: bool,
    ) -> Result<BrowserSession> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let s = session_in(&mut tx, sid).await?;
        if !s.preview() {
            return Err(StoreError::NotFound);
        }
        if s.runner.as_deref() != Some(runner.name.as_str()) {
            return Err(StoreError::Refused(
                "that preview is not on this runner".into(),
            ));
        }
        if s.is_open() {
            end_in(&mut tx, &s, if failed { "failed" } else { "closed" }).await?;
        }
        let s = session_in(&mut tx, sid).await?;
        tx.commit().await?;
        Ok(s)
    }

    /// Ends previews gone stale, for their screens to be told.
    pub(crate) async fn close_stale_previews(&self) -> Result<Vec<Ended>> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let ended = close_stale(&mut tx).await?;
        tx.commit().await?;
        Ok(ended)
    }

    /// The previews waiting for the PC or running now (at most one).
    pub async fn open_previews(&self) -> Result<Vec<BrowserSession>> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        close_stale(&mut tx).await?;
        let rows = sqlx::query(&format!(
            "SELECT {COLS} FROM browser_sessions
             WHERE kind = 'preview' AND state IN ('pending', 'live') ORDER BY id"
        ))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.iter().map(session_from).collect()
    }
}

// ---------- Handlers ----------

/// Tells the screens of previews that ended on their own why.
fn tell_ended(st: &AppState, ended: &[Ended]) {
    for (sid, why) in ended {
        st.browser.end(*sid, why);
    }
}

async fn start(st: AppState, target: PreviewTarget) -> Response {
    match st.store.start_preview(target).await {
        Ok(s) => {
            tell_ended(&st, &s.stale);
            for old in &s.replaced {
                st.browser.end(*old, REPLACED);
            }
            Json(s.session).into_response()
        }
        Err(e) => ApiError(e).into_response(),
    }
}

/// The owner's Run it on a piece of work.
async fn start_thread(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("running an app");
    }
    start(st, PreviewTarget::Thread(id)).await
}

/// The owner's Run it on an app.
async fn start_app(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("running an app");
    }
    start(st, PreviewTarget::App(id)).await
}

/// The owner's Stop. Stopping one already over changes nothing.
async fn stop(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(sid): Path<i64>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("stopping a preview");
    }
    match st.store.browser_session(sid).await {
        Ok(s) if s.preview() => {}
        Ok(_) => {
            return ApiError(StoreError::Refused(
                "Only a preview can be stopped here.".into(),
            ))
            .into_response()
        }
        Err(e) => return ApiError(e).into_response(),
    }
    if let Err(e) = st.store.end_browser(sid, "closed").await {
        return ApiError(e).into_response();
    }
    st.browser.end(sid, STOPPED);
    match st.store.browser_session(sid).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn list(State(st): State<AppState>, Extension(c): Extension<Caller>) -> Response {
    if c != Caller::Owner {
        return forbidden("seeing previews");
    }
    match st.store.close_stale_previews().await {
        Ok(ended) => tell_ended(&st, &ended),
        Err(e) => return ApiError(e).into_response(),
    }
    match st.store.open_previews().await {
        Ok(v) => Json(v).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// Previews run on the owner's PC only. A cloud worker's key never reaches
/// these routes anyway (`cloud_reach`).
// Responses are what axum handlers return; boxing them buys nothing here.
#[allow(clippy::result_large_err)]
fn pc_runner(runner: Option<Extension<RunnerId>>) -> std::result::Result<RunnerId, Response> {
    let r = runner_only(runner, None)?;
    if r.place != Place::Pc {
        return Err(forbidden("previews run on the PC; this"));
    }
    Ok(r)
}

async fn runner_claim(State(st): State<AppState>, runner: Option<Extension<RunnerId>>) -> Response {
    let r = match pc_runner(runner) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match st.store.claim_preview_ending(&r).await {
        Ok((claim, ended)) => {
            tell_ended(&st, &ended);
            match claim {
                Some(c) => Json(c).into_response(),
                None => StatusCode::NO_CONTENT.into_response(),
            }
        }
        Err(e) => ApiError(e).into_response(),
    }
}

/// The runner's "still running" (its body, a `PreviewAlive`, says nothing
/// more yet): the answer says whether to keep going. A preview nobody has
/// had open for the idle limit is stopped here.
async fn runner_alive(
    State(st): State<AppState>,
    runner: Option<Extension<RunnerId>>,
    Path(sid): Path<i64>,
) -> Response {
    let r = match pc_runner(runner) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let open = match st.store.preview_alive(sid, &r).await {
        Ok(open) => open,
        Err(e) => return ApiError(e).into_response(),
    };
    if !open {
        return Json(PreviewState { open: false }).into_response();
    }
    let s = match st.store.browser_session(sid).await {
        Ok(s) => s,
        Err(e) => return ApiError(e).into_response(),
    };
    if st.browser.idle_for(&s) < st.browser.preview_idle {
        return Json(PreviewState { open: true }).into_response();
    }
    if let Err(e) = st.store.end_browser(sid, "closed").await {
        return ApiError(e).into_response();
    }
    st.browser.end(sid, IDLE);
    Json(PreviewState { open: false }).into_response()
}

/// The runner ends a preview it holds, before its relay is up or without
/// one: the owner's screen is told why.
async fn runner_end(
    State(st): State<AppState>,
    runner: Option<Extension<RunnerId>>,
    Path(sid): Path<i64>,
    Json(e): Json<PreviewEnded>,
) -> Response {
    let r = match pc_runner(runner) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match st.store.end_preview_by_runner(sid, &r, e.failed).await {
        Ok(s) => {
            let why: String = e.why.chars().take(MAX_WHY).collect();
            st.browser.end(sid, &why);
            Json(s).into_response()
        }
        Err(e) => ApiError(e).into_response(),
    }
}
