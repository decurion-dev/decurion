//! Watching, and taking over, the agent's test browser from the owner's
//! screen. The same live view is the window the owner signs in to a site
//! with (see `kind = 'sign_in'`).
//!
//! The browser runs on the runner's computer. While the owner watches, the
//! runner opens a WebSocket to the hub and sends the screen as JPEG frames;
//! the owner's screen opens another, and the hub passes frames one way and
//! the owner's taps and typing the other. The hub only relays: frames and
//! input are never written down or logged. A frame goes to the owner's
//! screen only once the one before it has been shown, so a slow phone gets
//! the newest picture rather than a growing backlog.
//!
//! "Take over" gives the owner the browser and tells the agent, with a note,
//! to leave it alone; "Hand back" returns it with another note. It goes back
//! on its own when the owner closes the view or stops touching it for
//! [`IDLE_HAND_BACK_SECS`].

use crate::messages::add_message_in;
use crate::store::{state_in, Result, Store, StoreError, OWNER};
use crate::workers::RunnerId;
use crate::{forbidden, runner_only, ApiError, AppState, Caller, DeviceId};
use axum::body::Bytes;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message as WsMessage, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use decurion_proto::browser::{
    BrowserWant, Controller, Input, RelayDown, RelayUp, ViewerDown, ViewerUp, IDLE_HAND_BACK_SECS,
    MAX_FRAME_BYTES, MAX_INPUT_BYTES,
};
use decurion_proto::{MessageKind, NewMessage, ThreadState};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, Transaction};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// What the agent is told when the owner takes the browser.
pub const TAKEN_NOTE: &str =
    "The owner took over the test browser. Don't use the browser tools until it's handed back.";
/// What the agent is told when it gets the browser back.
pub const HANDED_BACK_NOTE: &str =
    "The owner handed the test browser back. Take a fresh snapshot before using it.";

const ENDED: &str = "This live view has ended.";
const WAITING: &str = "Waiting for the test browser. It can take a few seconds to start.";
const AWAY: &str = "The test browser stopped sending for now. Waiting for it to come back.";

/// Room for the JSON around a frame's picture.
const ENVELOPE: usize = 4 << 10;
/// Frames are held back by the one-in-flight rule, so a few is plenty.
const VIEWER_QUEUE: usize = 8;
const RUNNER_QUEUE: usize = 64;
/// Keeps idle connections open through proxies, and finds dead ones.
const PING_EVERY: Duration = Duration::from_secs(20);
/// How wide frames may be asked for, in pixels.
const WIDTHS: (u32, u32, u32) = (320, 1280, 1920);
/// The named keys the owner's screen may press.
const KEYS: &[&str] = &[
    "Enter",
    "Tab",
    "Backspace",
    "Escape",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
];

/// The browser routes, merged into the `/api` router behind the same keys.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/threads/{id}/browser/watch", post(watch))
        .route("/browser/{sid}/ws", get(viewer_socket))
        .route("/browser/{sid}/take", post(take))
        .route("/browser/{sid}/give-back", post(give_back))
        .route("/runner/browser/{sid}/ws", get(runner_socket))
}

// ---------- What the database keeps ----------

/// One live view, as the database keeps it. Never the picture or the input.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BrowserSession {
    pub id: i64,
    /// `take_over` or `sign_in`.
    pub kind: String,
    pub thread_id: Option<i64>,
    pub sign_in_id: Option<i64>,
    /// The runner holding the browser, by name.
    pub runner: Option<String>,
    pub controller: Controller,
    /// `pending`, `live`, `saved`, `closed` or `failed`.
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
}

impl BrowserSession {
    pub fn is_open(&self) -> bool {
        matches!(self.state.as_str(), "pending" | "live")
    }

    fn take_over(&self) -> bool {
        self.kind == "take_over"
    }
}

/// What the owner's "Watch the browser" got: the session, and the one it
/// replaced if the run had moved to another runner.
#[derive(Debug, Clone)]
pub struct Watch {
    pub session: BrowserSession,
    pub replaced: Option<i64>,
}

const COLS: &str =
    "id, kind, thread_id, sign_in_id, runner, controller, state, created_at, updated_at";
const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')";

fn session_from(row: &SqliteRow) -> Result<BrowserSession> {
    let controller: String = row.try_get("controller")?;
    Ok(BrowserSession {
        id: row.try_get("id")?,
        kind: row.try_get("kind")?,
        thread_id: row.try_get("thread_id")?,
        sign_in_id: row.try_get("sign_in_id")?,
        runner: row.try_get("runner")?,
        controller: if controller == "owner" {
            Controller::Owner
        } else {
            Controller::Agent
        },
        state: row.try_get("state")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn who(c: Controller) -> &'static str {
    match c {
        Controller::Agent => "agent",
        Controller::Owner => "owner",
    }
}

async fn session_in(tx: &mut Transaction<'_, Sqlite>, id: i64) -> Result<BrowserSession> {
    let row = sqlx::query(&format!("SELECT {COLS} FROM browser_sessions WHERE id = ?"))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(StoreError::NotFound)?;
    session_from(&row)
}

/// Tells the agent of a take-over view what happened, as a note from the
/// owner. Finished work has no agent left to tell.
async fn note_in(tx: &mut Transaction<'_, Sqlite>, s: &BrowserSession, text: &str) -> Result<()> {
    let Some(thread) = s.thread_id.filter(|_| s.take_over()) else {
        return Ok(());
    };
    if state_in(tx, thread).await? == ThreadState::Done {
        return Ok(());
    }
    let note = NewMessage {
        kind: MessageKind::Note,
        text: text.to_owned(),
        receipt: None,
        accept_on_yes: false,
    };
    // A note never sends work back, so the reason is never used.
    add_message_in(tx, thread, &note, OWNER, "a note about the test browser").await?;
    Ok(())
}

/// Ends an open session, giving the browser back to the agent first if the
/// owner had it.
async fn end_in(tx: &mut Transaction<'_, Sqlite>, s: &BrowserSession, state: &str) -> Result<()> {
    if s.controller == Controller::Owner {
        note_in(tx, s, HANDED_BACK_NOTE).await?;
    }
    sqlx::query(&format!(
        "UPDATE browser_sessions SET state = ?, controller = 'agent', updated_at = {NOW},
             ended_at = {NOW}
         WHERE id = ? AND state IN ('pending', 'live')"
    ))
    .bind(state)
    .bind(s.id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

impl Store {
    pub async fn browser_session(&self, id: i64) -> Result<BrowserSession> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let s = session_in(&mut tx, id).await?;
        tx.commit().await?;
        Ok(s)
    }

    /// The take-over view of a running Claude run, reused if one is open on
    /// the runner holding the run, otherwise made new.
    pub async fn watch_browser(&self, thread_id: i64) -> Result<Watch> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let row: Option<(String, String, Option<String>)> =
            sqlx::query_as("SELECT state, ai, runner FROM threads WHERE id = ?")
                .bind(thread_id)
                .fetch_optional(&mut *tx)
                .await?;
        let (state, ai, holder) = row.ok_or(StoreError::NotFound)?;
        if ai != "claude" {
            return Err(StoreError::Refused(
                "only Claude's work has a test browser to watch".into(),
            ));
        }
        let runner = holder
            .as_deref()
            .and_then(|h| h.strip_prefix("runner:"))
            .filter(|_| state == "running")
            .ok_or_else(|| {
                StoreError::Refused(
                    "the test browser can only be watched while the agent is working".into(),
                )
            })?
            .to_owned();
        let open = sqlx::query(&format!(
            "SELECT {COLS} FROM browser_sessions
             WHERE thread_id = ? AND kind = 'take_over' AND state IN ('pending', 'live')"
        ))
        .bind(thread_id)
        .fetch_optional(&mut *tx)
        .await?;
        let mut replaced = None;
        if let Some(row) = open {
            let s = session_from(&row)?;
            if s.runner.as_deref() == Some(runner.as_str()) {
                tx.commit().await?;
                return Ok(Watch {
                    session: s,
                    replaced: None,
                });
            }
            // The run moved to another computer, and its browser with it.
            end_in(&mut tx, &s, "closed").await?;
            replaced = Some(s.id);
        }
        let row = sqlx::query(&format!(
            "INSERT INTO browser_sessions (kind, thread_id, runner) VALUES ('take_over', ?, ?)
             RETURNING {COLS}"
        ))
        .bind(thread_id)
        .bind(&runner)
        .fetch_one(&mut *tx)
        .await?;
        let session = session_from(&row)?;
        tx.commit().await?;
        Ok(Watch { session, replaced })
    }

    /// Gives the browser of a take-over view to the owner or back to the
    /// agent, telling the agent with a note. `None` if it already was.
    pub async fn set_browser_controller(
        &self,
        id: i64,
        to: Controller,
    ) -> Result<Option<BrowserSession>> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let s = session_in(&mut tx, id).await?;
        if !s.take_over() {
            return Err(StoreError::Refused(
                "only the agent's browser is taken over and handed back".into(),
            ));
        }
        if s.controller == to || (!s.is_open() && to == Controller::Agent) {
            return Ok(None);
        }
        if !s.is_open() {
            return Err(StoreError::Refused(ENDED.into()));
        }
        if to == Controller::Owner {
            let thread = s.thread_id.ok_or(StoreError::NotFound)?;
            if state_in(&mut tx, thread).await? != ThreadState::Running {
                return Err(StoreError::Refused(
                    "the agent is not working on this any more".into(),
                ));
            }
        }
        let note = match to {
            Controller::Owner => TAKEN_NOTE,
            Controller::Agent => HANDED_BACK_NOTE,
        };
        note_in(&mut tx, &s, note).await?;
        sqlx::query(&format!(
            "UPDATE browser_sessions SET controller = ?, updated_at = {NOW} WHERE id = ?"
        ))
        .bind(who(to))
        .bind(id)
        .execute(&mut *tx)
        .await?;
        let s = session_in(&mut tx, id).await?;
        tx.commit().await?;
        Ok(Some(s))
    }

    /// The session a runner asks to relay, if it is open and the runner holds
    /// it: for a take-over view, the runner must be running that work now.
    pub async fn browser_for_runner(&self, id: i64, runner: &str) -> Result<BrowserSession> {
        let s = self.browser_session(id).await?;
        if !s.is_open() {
            return Err(StoreError::Refused(ENDED.into()));
        }
        if s.runner.as_deref() != Some(runner) {
            return Err(StoreError::Refused(
                "that browser is not on this runner".into(),
            ));
        }
        if let (true, Some(t)) = (s.take_over(), s.thread_id) {
            if !self.running_on(t, runner).await? {
                return Err(StoreError::Refused(
                    "that work is not running on this runner".into(),
                ));
            }
        }
        Ok(s)
    }

    /// Marks an open session live (its runner is connected) or pending.
    pub async fn browser_connected(&self, id: i64, up: bool) -> Result<()> {
        let (to, from) = if up {
            ("live", "pending")
        } else {
            ("pending", "live")
        };
        sqlx::query(&format!(
            "UPDATE browser_sessions SET state = ?, updated_at = {NOW} WHERE id = ? AND state = ?"
        ))
        .bind(to)
        .bind(id)
        .bind(from)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Ends a session as `closed` or `failed`, handing the browser back
    /// first. Returns it as it was, if it was still open.
    pub async fn end_browser(&self, id: i64, state: &str) -> Result<Option<BrowserSession>> {
        if !matches!(state, "closed" | "failed") {
            return Err(StoreError::Invalid(
                "a session ends closed or failed".into(),
            ));
        }
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let s = session_in(&mut tx, id).await?;
        if !s.is_open() {
            return Ok(None);
        }
        end_in(&mut tx, &s, state).await?;
        tx.commit().await?;
        Ok(Some(s))
    }
}

// ---------- The relay ----------

fn line<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// One end of a relay: the owner's screen or the runner. Dropping it closes
/// the connection once what was already queued has gone out.
struct Peer {
    conn: u64,
    tx: mpsc::Sender<String>,
}

impl Peer {
    /// Queues a message; false if the connection is too far behind or gone.
    fn send(&self, text: String) -> bool {
        self.tx.try_send(text).is_ok()
    }
}

/// What is going on in one live view right now. Holds at most one frame
/// waiting to be shown, never a backlog.
struct Relay {
    thread_id: Option<i64>,
    take_over: bool,
    controller: Controller,
    viewer: Option<Peer>,
    runner: Option<Peer>,
    max_width: u32,
    /// The frame the owner's screen has not acknowledged yet.
    in_flight: Option<u64>,
    /// The newest frame that came while one was in flight; each newer one
    /// replaces it.
    waiting: Option<(u64, String)>,
    /// The page shown, as the browser has it, for a screen that joins late.
    page: Option<String>,
    last_input: Instant,
    /// Counts each change of who drives, so an old idle timer stands down.
    generation: u64,
}

impl Relay {
    fn new(s: &BrowserSession) -> Self {
        Self {
            thread_id: s.thread_id,
            take_over: s.take_over(),
            controller: s.controller,
            viewer: None,
            runner: None,
            max_width: WIDTHS.1,
            in_flight: None,
            waiting: None,
            page: None,
            last_input: Instant::now(),
            generation: 0,
        }
    }

    fn to_runner(&self, m: &RelayDown) -> bool {
        self.runner.as_ref().is_some_and(|p| p.send(line(m)))
    }

    fn to_viewer(&self, m: &ViewerDown) -> bool {
        self.viewer.as_ref().is_some_and(|p| p.send(line(m)))
    }

    /// Lets the browser send its next frame.
    fn ack(&self, seq: u64) {
        self.to_runner(&RelayDown::FrameAck { seq });
    }

    /// Forgets frames on their way to a screen that is gone, so the browser
    /// does not wait for them.
    fn drop_frames(&mut self) {
        let waiting = self.waiting.take().map(|(seq, _)| seq);
        for seq in self.in_flight.take().into_iter().chain(waiting) {
            self.ack(seq);
        }
    }

    /// Sends a frame to the screen, or acknowledges it straight away if it
    /// cannot go.
    fn show(&mut self, seq: u64, text: String) {
        if self.viewer.as_ref().is_some_and(|v| v.send(text)) {
            self.in_flight = Some(seq);
        } else {
            self.ack(seq);
        }
    }
}

/// Every live view going on now, by session id.
#[derive(Clone)]
pub struct Relays {
    map: Arc<Mutex<HashMap<i64, Relay>>>,
    conns: Arc<AtomicU64>,
    /// See [`IDLE_HAND_BACK_SECS`]; tests shorten it.
    hand_back_after: Duration,
}

impl Default for Relays {
    fn default() -> Self {
        Self {
            map: Arc::default(),
            conns: Arc::default(),
            hand_back_after: Duration::from_secs(IDLE_HAND_BACK_SECS),
        }
    }
}

impl AppState {
    /// How long the owner may hold the test browser without touching it
    /// before it goes back to the agent. Normally [`IDLE_HAND_BACK_SECS`].
    pub fn with_hand_back_after(mut self, d: Duration) -> Self {
        self.browser.hand_back_after = d;
        self
    }
}

impl Relays {
    fn lock(&self) -> MutexGuard<'_, HashMap<i64, Relay>> {
        // Nothing here can be left half-changed by a panic, so a poisoned
        // lock is still fine to use.
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn with<R>(&self, sid: i64, f: impl FnOnce(&mut Relay) -> R) -> Option<R> {
        self.lock().get_mut(&sid).map(f)
    }

    fn entry<R>(&self, s: &BrowserSession, f: impl FnOnce(&mut Relay) -> R) -> R {
        let mut map = self.lock();
        f(map.entry(s.id).or_insert_with(|| Relay::new(s)))
    }

    /// Forgets a view nobody is connected to, unless the owner still has
    /// the browser (the idle timer needs it then).
    fn tidy(&self, sid: i64) {
        let mut map = self.lock();
        let idle = map.get(&sid).is_some_and(|r| {
            r.viewer.is_none() && r.runner.is_none() && r.controller == Controller::Agent
        });
        if idle {
            map.remove(&sid);
        }
    }

    /// A new connection's number, so a replaced one can tell it is not
    /// current any more.
    fn next_conn(&self) -> u64 {
        self.conns.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// The take-over view the owner is watching for this run, for the
    /// runner's live check.
    pub fn want(&self, thread_id: i64) -> Option<BrowserWant> {
        self.lock()
            .iter()
            .find(|(_, r)| r.take_over && r.thread_id == Some(thread_id) && r.viewer.is_some())
            .map(|(sid, _)| BrowserWant { session_id: *sid })
    }

    /// Sends the runner a message, if it is connected. For the sign-in
    /// window's "I'm signed in, save" ([`RelayDown::Capture`]).
    pub fn send_to_runner(&self, sid: i64, m: &RelayDown) -> bool {
        self.with(sid, |r| r.to_runner(m)).unwrap_or(false)
    }

    /// Connects the owner's screen, replacing any other screen on the same
    /// view. Returns its connection, and the idle timer to start if the
    /// owner already has the browser.
    fn attach_viewer(
        &self,
        s: &BrowserSession,
        width: u32,
    ) -> (u64, mpsc::Receiver<String>, Option<u64>) {
        let conn = self.next_conn();
        let (tx, rx) = mpsc::channel(VIEWER_QUEUE);
        let idle = self.entry(s, |r| {
            if let Some(old) = r.viewer.take() {
                old.send(line(&ViewerDown::Ended {
                    why: "This live view was opened on another screen.".into(),
                }));
            }
            r.drop_frames();
            r.max_width = width;
            let p = Peer { conn, tx };
            p.send(line(&ViewerDown::Control { who: r.controller }));
            if let Some(page) = &r.page {
                p.send(page.clone());
            }
            if r.runner.is_none() {
                p.send(line(&ViewerDown::Ended {
                    why: WAITING.into(),
                }));
            }
            r.viewer = Some(p);
            r.to_runner(&RelayDown::Watch {
                on: true,
                max_width: width,
            });
            (r.take_over && r.controller == Controller::Owner).then_some(r.generation)
        });
        (conn, rx, idle)
    }

    /// Disconnects the owner's screen. True if it was the current one and
    /// the owner had the browser, which then goes back to the agent.
    fn detach_viewer(&self, sid: i64, conn: u64) -> bool {
        let owner = self
            .with(sid, |r| {
                if r.viewer.as_ref().map(|p| p.conn) != Some(conn) {
                    return false;
                }
                r.viewer = None;
                r.drop_frames();
                r.to_runner(&RelayDown::Watch {
                    on: false,
                    max_width: r.max_width,
                });
                r.take_over && r.controller == Controller::Owner
            })
            .unwrap_or(false);
        self.tidy(sid);
        owner
    }

    /// Connects the runner, replacing an older connection of it. Returns its
    /// connection, and whether the owner holds the browser with nobody
    /// watching (left that way by a hub restart), which needs handing back.
    fn attach_runner(&self, s: &BrowserSession) -> (u64, mpsc::Receiver<String>, bool) {
        let conn = self.next_conn();
        let (tx, rx) = mpsc::channel(RUNNER_QUEUE);
        let orphaned = self.entry(s, |r| {
            r.runner = Some(Peer { conn, tx });
            r.drop_frames();
            r.to_runner(&RelayDown::Control { who: r.controller });
            if r.viewer.is_some() {
                r.to_runner(&RelayDown::Watch {
                    on: true,
                    max_width: r.max_width,
                });
            }
            r.take_over && r.controller == Controller::Owner && r.viewer.is_none()
        });
        (conn, rx, orphaned)
    }

    /// Disconnects the runner. True if it was the current connection.
    fn detach_runner(&self, sid: i64, conn: u64) -> bool {
        let current = self
            .with(sid, |r| {
                if r.runner.as_ref().map(|p| p.conn) != Some(conn) {
                    return false;
                }
                r.runner = None;
                r.in_flight = None;
                r.waiting = None;
                true
            })
            .unwrap_or(false);
        self.tidy(sid);
        current
    }

    /// A frame from the runner: shown if the screen is ready for it, kept as
    /// the next one if not (replacing an older one), dropped if nobody is
    /// watching.
    fn frame(&self, sid: i64, seq: u64, text: String) {
        self.with(sid, |r| {
            if r.viewer.is_none() {
                r.ack(seq);
            } else if r.in_flight.is_none() {
                r.show(seq, text);
            } else if let Some((older, _)) = r.waiting.replace((seq, text)) {
                r.ack(older);
            }
        });
    }

    /// A frame too big to pass on.
    fn refuse_frame(&self, sid: i64, seq: u64) {
        self.with(sid, |r| r.ack(seq));
    }

    /// The owner's screen has shown a frame: the browser may send another,
    /// and the one waiting goes out.
    fn viewer_ack(&self, sid: i64, seq: u64) {
        self.with(sid, |r| {
            if r.in_flight != Some(seq) {
                return;
            }
            r.in_flight = None;
            r.ack(seq);
            if let Some((next, text)) = r.waiting.take() {
                r.show(next, text);
            }
        });
    }

    fn page(&self, sid: i64, text: String) {
        self.with(sid, |r| {
            if let Some(v) = &r.viewer {
                v.send(text.clone());
            }
            r.page = Some(text);
        });
    }

    /// The owner's input, passed on only while the owner has the browser.
    fn input(&self, sid: i64, input: Input) -> bool {
        if !input_ok(&input) {
            return false;
        }
        self.with(sid, |r| {
            if r.controller != Controller::Owner {
                return false;
            }
            r.last_input = Instant::now();
            r.to_runner(&RelayDown::Input { input })
        })
        .unwrap_or(false)
    }

    /// Tells both ends who drives now. Returns the new generation, for the
    /// idle timer.
    fn set_controller(&self, s: &BrowserSession) -> u64 {
        let generation = self.entry(s, |r| {
            r.controller = s.controller;
            r.generation += 1;
            r.last_input = Instant::now();
            r.to_runner(&RelayDown::Control { who: s.controller });
            r.to_viewer(&ViewerDown::Control { who: s.controller });
            r.generation
        });
        self.tidy(s.id);
        generation
    }

    /// When the owner's control runs out, if it is still the control that
    /// `generation` started.
    fn idle_deadline(&self, sid: i64, generation: u64) -> Option<Instant> {
        let after = self.hand_back_after;
        self.with(sid, |r| {
            (r.generation == generation && r.controller == Controller::Owner)
                .then_some(r.last_input + after)
        })
        .flatten()
    }

    /// The view is over: the screen is told why, and both ends are closed.
    fn end(&self, sid: i64, why: &str) {
        if let Some(r) = self.lock().remove(&sid) {
            r.to_viewer(&ViewerDown::Ended { why: why.into() });
        }
    }
}

/// Whether a piece of the owner's input makes sense; the browser box checks
/// again.
fn input_ok(i: &Input) -> bool {
    let frac = |v: f64| (0.0..=1.0).contains(&v);
    // Scrolls are fractions of the picture, at most ten pictures at once.
    let step = |v: f64| v.is_finite() && v.abs() <= 10.0;
    match i {
        Input::Click { x, y } => frac(*x) && frac(*y),
        Input::Scroll { x, y, dx, dy } => frac(*x) && frac(*y) && step(*dx) && step(*dy),
        Input::Text { text } => !text.is_empty() && text.len() <= MAX_INPUT_BYTES,
        Input::Key { key } => KEYS.contains(&key.as_str()),
        Input::Go { url } => {
            url.len() <= 2048 && (url.starts_with("https://") || url.starts_with("http://"))
        }
        Input::Back => true,
    }
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// One WebSocket and what is queued for it.
struct Conn {
    socket: WebSocket,
    rx: mpsc::Receiver<String>,
    ping: tokio::time::Interval,
}

impl Conn {
    fn new(socket: WebSocket, rx: mpsc::Receiver<String>) -> Self {
        let start = tokio::time::Instant::now() + PING_EVERY;
        let mut ping = tokio::time::interval_at(start, PING_EVERY);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Self { socket, rx, ping }
    }

    /// The other end's next text message, sending what is queued for it
    /// meanwhile. `None` once either side is done.
    async fn next(&mut self) -> Option<Utf8Bytes> {
        loop {
            tokio::select! {
                m = self.socket.recv() => match m {
                    Some(Ok(WsMessage::Text(t))) => return Some(t),
                    Some(Ok(WsMessage::Close(_)) | Err(_)) | None => return None,
                    Some(Ok(_)) => {}
                },
                out = self.rx.recv() => {
                    let Some(text) = out else {
                        let _ = self.socket.send(WsMessage::Close(None)).await;
                        return None;
                    };
                    if self.socket.send(WsMessage::text(text)).await.is_err() {
                        return None;
                    }
                }
                _ = self.ping.tick() => {
                    if self.socket.send(WsMessage::Ping(Bytes::new())).await.is_err() {
                        return None;
                    }
                }
            }
        }
    }
}

// ---------- Handing the browser back and forth ----------

async fn hand_back(st: &AppState, sid: i64) {
    match st
        .store
        .set_browser_controller(sid, Controller::Agent)
        .await
    {
        Ok(Some(s)) => {
            st.browser.set_controller(&s);
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, session = sid, "could not hand the test browser back"),
    }
}

/// Hands the browser back once the owner has not touched it for a while.
async fn hand_back_when_idle(st: AppState, sid: i64, generation: u64) {
    while let Some(at) = st.browser.idle_deadline(sid, generation) {
        if Instant::now() >= at {
            hand_back(&st, sid).await;
            return;
        }
        tokio::time::sleep_until(at.into()).await;
    }
}

/// Ends a view for good: the database first, then the open connections.
async fn end(st: &AppState, sid: i64, why: &str) {
    if let Err(e) = st.store.end_browser(sid, "closed").await {
        tracing::warn!(error = %e, session = sid, "could not close a browser view");
    }
    st.browser.end(sid, why);
}

// ---------- Handlers ----------

/// The owner's "Watch the browser" on running Claude work.
async fn watch(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("watching the test browser");
    }
    match st.store.watch_browser(id).await {
        Ok(w) => {
            if let Some(old) = w.replaced {
                st.browser
                    .end(old, "This work moved to another computer. Open it again.");
            }
            Json(w.session).into_response()
        }
        Err(e) => ApiError(e).into_response(),
    }
}

async fn take(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(sid): Path<i64>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("taking over the test browser");
    }
    match st
        .store
        .set_browser_controller(sid, Controller::Owner)
        .await
    {
        Ok(Some(s)) => {
            let generation = st.browser.set_controller(&s);
            tokio::spawn(hand_back_when_idle(st.clone(), sid, generation));
            Json(s).into_response()
        }
        Ok(None) => current(&st, sid).await,
        Err(e) => ApiError(e).into_response(),
    }
}

async fn give_back(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(sid): Path<i64>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("handing back the test browser");
    }
    match st
        .store
        .set_browser_controller(sid, Controller::Agent)
        .await
    {
        Ok(Some(s)) => {
            st.browser.set_controller(&s);
            Json(s).into_response()
        }
        Ok(None) => current(&st, sid).await,
        Err(e) => ApiError(e).into_response(),
    }
}

async fn current(st: &AppState, sid: i64) -> Response {
    match st.store.browser_session(sid).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// A cookie is sent with a WebSocket from any page on the same host, and
/// browsers do not hold WebSockets to the same-origin rule, so the owner's
/// screen must come from the hub's own pages. Browsers always say where a
/// WebSocket was opened from.
fn same_origin_socket(h: &HeaderMap) -> bool {
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

#[derive(Deserialize)]
struct ViewQuery {
    /// How wide the screen can show the browser, in device pixels.
    max_width: Option<u32>,
}

/// The owner's screen: frames, the page address and who drives come down;
/// acknowledgements and input go up.
async fn viewer_socket(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Extension(DeviceId(device)): Extension<DeviceId>,
    headers: HeaderMap,
    Path(sid): Path<i64>,
    Query(q): Query<ViewQuery>,
    ws: std::result::Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("watching the test browser");
    }
    if device.is_some() && !same_origin_socket(&headers) {
        return (StatusCode::FORBIDDEN, "cross-site request refused").into_response();
    }
    let s = match st.store.browser_session(sid).await {
        Ok(s) if s.is_open() => s,
        Ok(_) => return (StatusCode::CONFLICT, ENDED).into_response(),
        Err(e) => return ApiError(e).into_response(),
    };
    let ws = match ws {
        Ok(ws) => ws,
        Err(e) => return e.into_response(),
    };
    let width = q.max_width.unwrap_or(WIDTHS.1).clamp(WIDTHS.0, WIDTHS.2);
    // Anything bigger than one piece of input is not from the owner's screen.
    ws.max_message_size(MAX_INPUT_BYTES + ENVELOPE)
        .on_upgrade(move |socket| relay_viewer(st, s, width, socket))
}

async fn relay_viewer(st: AppState, s: BrowserSession, width: u32, socket: WebSocket) {
    let (conn, rx, idle) = st.browser.attach_viewer(&s, width);
    if let Some(generation) = idle {
        tokio::spawn(hand_back_when_idle(st.clone(), s.id, generation));
    }
    let mut c = Conn::new(socket, rx);
    while let Some(text) = c.next().await {
        if text.len() > MAX_INPUT_BYTES {
            continue;
        }
        // Never logged, even when it does not parse: it could be typing.
        match serde_json::from_str::<ViewerUp>(text.as_str()) {
            Ok(ViewerUp::FrameAck { seq }) => st.browser.viewer_ack(s.id, seq),
            Ok(ViewerUp::Input { input }) => {
                st.browser.input(s.id, input);
            }
            Err(_) => {}
        }
    }
    if st.browser.detach_viewer(s.id, conn) {
        hand_back(&st, s.id).await;
    }
}

/// The runner holding the browser: frames and the page address come up;
/// acknowledgements, input and who drives go down.
async fn runner_socket(
    State(st): State<AppState>,
    runner: Option<Extension<RunnerId>>,
    Path(sid): Path<i64>,
    ws: std::result::Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let r = match runner_only(runner, None) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let s = match st.store.browser_for_runner(sid, &r.name).await {
        Ok(s) => s,
        Err(e) => return ApiError(e).into_response(),
    };
    let ws = match ws {
        Ok(ws) => ws,
        Err(e) => return e.into_response(),
    };
    ws.max_message_size(MAX_FRAME_BYTES + ENVELOPE)
        .on_upgrade(move |socket| relay_runner(st, s, socket))
}

async fn relay_runner(st: AppState, s: BrowserSession, socket: WebSocket) {
    let (conn, rx, orphaned) = st.browser.attach_runner(&s);
    if let Err(e) = st.store.browser_connected(s.id, true).await {
        tracing::warn!(error = %e, session = s.id, "could not mark a browser view live");
    }
    if orphaned {
        hand_back(&st, s.id).await;
    }
    let mut c = Conn::new(socket, rx);
    let mut ended = None;
    while let Some(text) = c.next().await {
        match serde_json::from_str::<RelayUp>(text.as_str()) {
            Ok(RelayUp::Frame {
                seq,
                jpeg,
                width,
                height,
            }) => {
                if jpeg.len() > MAX_FRAME_BYTES {
                    tracing::warn!(session = s.id, bytes = jpeg.len(), "frame too big, dropped");
                    st.browser.refuse_frame(s.id, seq);
                    continue;
                }
                let down = ViewerDown::Frame {
                    seq,
                    jpeg,
                    width,
                    height,
                };
                st.browser.frame(s.id, seq, line(&down));
            }
            Ok(RelayUp::Page { url, title }) => {
                let down = ViewerDown::Page {
                    url: cut(&url, 2048),
                    title: cut(&title, 200),
                };
                st.browser.page(s.id, line(&down));
            }
            Ok(RelayUp::Ended { why }) => {
                ended = Some(cut(&why, 300));
                break;
            }
            Err(_) => {}
        }
    }
    drop(c);
    let current = st.browser.detach_runner(s.id, conn);
    if let Some(why) = ended {
        end(&st, s.id, &why).await;
    } else if current {
        st.browser.with(s.id, |r| {
            r.to_viewer(&ViewerDown::Ended { why: AWAY.into() });
        });
        if let Err(e) = st.store.browser_connected(s.id, false).await {
            tracing::warn!(error = %e, session = s.id, "could not mark a browser view pending");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_is_checked() {
        assert!(input_ok(&Input::Click { x: 0.5, y: 1.0 }));
        assert!(!input_ok(&Input::Click {
            x: f64::NAN,
            y: 0.1
        }));
        assert!(!input_ok(&Input::Click { x: 1.5, y: 0.1 }));
        assert!(!input_ok(&Input::Key { key: "F12".into() }));
        assert!(input_ok(&Input::Key {
            key: "Backspace".into()
        }));
        assert!(!input_ok(&Input::Go {
            url: "file:///etc/passwd".into()
        }));
        assert!(!input_ok(&Input::Text {
            text: "x".repeat(MAX_INPUT_BYTES + 1)
        }));
    }

    #[test]
    fn websockets_must_come_from_the_hubs_pages() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, "pc.tail.ts.net".parse().expect("host"));
        assert!(!same_origin_socket(&h));
        h.insert(header::ORIGIN, "https://evil.test".parse().expect("origin"));
        assert!(!same_origin_socket(&h));
        h.insert(
            header::ORIGIN,
            "https://pc.tail.ts.net".parse().expect("origin"),
        );
        assert!(same_origin_socket(&h));
        h.insert("sec-fetch-site", "same-site".parse().expect("site"));
        assert!(!same_origin_socket(&h));
    }
}
