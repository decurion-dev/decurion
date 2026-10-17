//! SQLite-backed board state. Every state change goes through [`checked_move`]
//! (or the claim path), which checks the allowed moves and writes an audit row
//! in the same transaction.

use decurion_proto::{
    App, Claim, EventRecord, Interrupted, NewApp, NewThread, Place, Profile, RunEvent, Thread,
    ThreadState,
};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use std::str::FromStr;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found")]
    NotFound,
    #[error("cannot move from {from} to {to}")]
    BadMove { from: ThreadState, to: ThreadState },
    #[error("thread changed underneath; reload and retry")]
    Conflict,
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Duplicate(String),
    /// The request makes sense but the thing it is about no longer allows it.
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// What the runner gets when it asks for work.
#[derive(Debug)]
pub struct ClaimOutcome {
    pub work: Option<Claim>,
    /// Pay-per-use work that did not fit in the month's cap and was moved to
    /// Needs you while looking for something to run.
    pub capped: Vec<Thread>,
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

const THREAD_COLS: &str =
    "id, app_id, title, prompt, state, session_id, branch, place, updated_at, parent_id, background";

/// The actor recorded for the owner's own moves.
pub const OWNER: &str = "owner";

/// A task with this many tasks above it cannot be split further. Deep trees
/// are hard to follow on a phone and usually mean an agent is looping.
pub const MAX_DEPTH: i64 = 5;

pub(crate) const RECEIPT_REUSED: &str = "this receipt was already used for something else";

fn thread_from(row: &SqliteRow) -> Result<Thread> {
    let state: String = row.try_get("state")?;
    let place: String = row.try_get("place")?;
    Ok(Thread {
        id: row.try_get("id")?,
        app_id: row.try_get("app_id")?,
        title: row.try_get("title")?,
        prompt: row.try_get("prompt")?,
        state: ThreadState::from_str(&state).map_err(|e| StoreError::Invalid(e.to_string()))?,
        session_id: row.try_get("session_id")?,
        branch: row.try_get("branch")?,
        place: Place::from_str(&place).map_err(|e| StoreError::Invalid(e.to_string()))?,
        updated_at: row.try_get("updated_at")?,
        parent_id: row.try_get("parent_id")?,
        background: row.try_get("background")?,
    })
}

pub(crate) fn app_from(row: &SqliteRow) -> Result<App> {
    let profile: String = row.try_get("profile")?;
    Ok(App {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        repo_path: row.try_get("repo_path")?,
        profile: Profile::from_str(&profile).map_err(|e| StoreError::Invalid(e.to_string()))?,
        pay_per_use: row.try_get("pay_per_use")?,
    })
}

impl Store {
    /// Opens (creating if needed) the database at `url`, e.g. `sqlite://hub.db`
    /// or `sqlite::memory:`, and applies migrations.
    pub async fn open(url: &str) -> Result<Self> {
        let opts = SqliteConnectOptions::from_str(url)?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal);
        // In-memory databases are per-connection, so tests use one connection.
        let max = if url.contains(":memory:") { 1 } else { 4 };
        let pool = SqlitePoolOptions::new()
            .max_connections(max)
            .connect_with(opts)
            .await?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|e| StoreError::Db(e.into()))?;
        Ok(Self { pool })
    }

    pub async fn add_app(&self, a: &NewApp) -> Result<App> {
        check_app_name(&a.name)?;
        check_repo_path(&a.repo_path)?;
        let row = sqlx::query(
            "INSERT INTO apps (name, repo_path, profile, pay_per_use) VALUES (?, ?, ?, ?)
             RETURNING id, name, repo_path, profile, pay_per_use",
        )
        .bind(&a.name)
        .bind(&a.repo_path)
        .bind(a.profile.as_str())
        .bind(a.pay_per_use)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| match e.as_database_error() {
            Some(d) if d.is_unique_violation() => {
                StoreError::Duplicate(format!("an app named {} already exists", a.name))
            }
            _ => StoreError::Db(e),
        })?;
        app_from(&row)
    }

    pub async fn apps(&self) -> Result<Vec<App>> {
        let rows =
            sqlx::query("SELECT id, name, repo_path, profile, pay_per_use FROM apps ORDER BY name")
                .fetch_all(&self.pool)
                .await?;
        rows.iter().map(app_from).collect()
    }

    pub async fn app(&self, id: i64) -> Result<App> {
        let row =
            sqlx::query("SELECT id, name, repo_path, profile, pay_per_use FROM apps WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?
                .ok_or(StoreError::NotFound)?;
        app_from(&row)
    }

    /// Adds work. With a receipt already used for adding work, nothing is
    /// added and the thread made the first time is returned; the flag is
    /// false then.
    pub async fn add_thread(&self, t: &NewThread) -> Result<(Thread, bool)> {
        if t.title.trim().is_empty() || t.prompt.trim().is_empty() {
            return Err(StoreError::Invalid("title and prompt are required".into()));
        }
        check_receipt(t.receipt.as_deref())?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(id) = receipt_result(&mut tx, t.receipt.as_deref(), "add_thread").await? {
            tx.commit().await?;
            return Ok((self.thread(id).await?, false));
        }
        let app: Option<i64> = sqlx::query_scalar("SELECT id FROM apps WHERE id = ?")
            .bind(t.app_id)
            .fetch_optional(&mut *tx)
            .await?;
        if app.is_none() {
            return Err(StoreError::NotFound);
        }
        if t.place == Place::Cloud && !crate::workers::cloud_worker_in(&mut tx).await? {
            return Err(StoreError::Refused(crate::workers::NO_CLOUD_WORKER.into()));
        }
        if let Some(parent) = t.parent_id {
            check_parent(&mut tx, parent, t.app_id).await?;
        }
        let row = sqlx::query(&format!(
            "INSERT INTO threads (app_id, title, prompt, parent_id, background, place)
             VALUES (?, ?, ?, ?, ?, ?) RETURNING {THREAD_COLS}"
        ))
        .bind(t.app_id)
        .bind(t.title.trim())
        .bind(t.prompt.trim())
        .bind(t.parent_id)
        .bind(t.background)
        .bind(t.place.as_str())
        .fetch_one(&mut *tx)
        .await?;
        let thread = thread_from(&row)?;
        save_receipt(&mut tx, t.receipt.as_deref(), "add_thread", thread.id).await?;
        tx.commit().await?;
        Ok((thread, true))
    }

    pub async fn threads(&self) -> Result<Vec<Thread>> {
        let rows = sqlx::query(&format!("SELECT {THREAD_COLS} FROM threads ORDER BY id"))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(thread_from).collect()
    }

    pub async fn thread(&self, id: i64) -> Result<Thread> {
        let row = sqlx::query(&format!("SELECT {THREAD_COLS} FROM threads WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)?;
        thread_from(&row)
    }

    /// Moves a thread to `to` if that move is allowed from its current state.
    /// With `expect`, the move only happens if the thread is still in that
    /// state. The update is conditional on the state read, so two racing moves
    /// cannot both succeed. A runner (actor `runner:<name>`) can only move work
    /// it is running itself, so one computer can never end another's run.
    pub async fn move_thread(
        &self,
        id: i64,
        expect: Option<ThreadState>,
        to: ThreadState,
        actor: &str,
        note: Option<&str>,
    ) -> Result<Thread> {
        Ok(self
            .request_move(id, expect, to, actor, note, None)
            .await?
            .0)
    }

    /// [`Store::move_thread`] as asked for from the board or the runner. Two
    /// moves do more than change the state:
    ///
    /// - The owner stopping work (to Failed) first stops the work under it,
    ///   deepest first, in the same transaction, so nothing is left running
    ///   for a task that was called off. Background work is left alone.
    /// - The runner finishing a run (Running to In review) while the owner
    ///   has a steer or follow-up waiting sends it back to the queue instead,
    ///   so the agent gets them before anyone reviews it.
    ///
    /// With a receipt already used for this move, nothing happens and the
    /// thread is returned as it is now; the flag is false then.
    pub async fn request_move(
        &self,
        id: i64,
        expect: Option<ThreadState>,
        to: ThreadState,
        actor: &str,
        note: Option<&str>,
        receipt: Option<&str>,
    ) -> Result<(Thread, bool)> {
        check_receipt(receipt)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(done) = receipt_result(&mut tx, receipt, "move").await? {
            if done != id {
                return Err(StoreError::Duplicate(RECEIPT_REUSED.into()));
            }
            tx.commit().await?;
            return Ok((self.thread(id).await?, false));
        }
        let from = state_in(&mut tx, id).await?;
        if expect.is_some_and(|e| e != from) {
            return Err(StoreError::Conflict);
        }
        if actor.starts_with("runner:") {
            let holder: Option<String> =
                sqlx::query_scalar("SELECT runner FROM threads WHERE id = ?")
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?;
            if from != ThreadState::Running || holder.as_deref() != Some(actor) {
                return Err(StoreError::Conflict);
            }
        }
        if !from.can_move_to(to) {
            return Err(StoreError::BadMove { from, to });
        }
        let (mut to, mut note) = (to, note);
        if actor == OWNER && to == ThreadState::Failed {
            stop_children(&mut tx, id).await?;
        } else if actor != OWNER
            && from == ThreadState::Running
            && to == ThreadState::InReview
            && crate::messages::owed(&mut tx, id).await?
        {
            to = ThreadState::Queued;
            note = Some("the owner sent more while it was finishing");
            // This run is over, so a move to pay-per-use made for it during
            // an outage ends here too, as it would on the way to review.
            sqlx::query("UPDATE threads SET pay_per_use = 0 WHERE id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        checked_move(&mut tx, id, Some(from), to, actor, note).await?;
        save_receipt(&mut tx, receipt, "move", id).await?;
        tx.commit().await?;
        Ok((self.thread(id).await?, true))
    }

    /// Hands the oldest queued thread to `runner` and marks it running. A thread
    /// whose agent already produced activity was paused mid-run, so the runner
    /// resumes that session; otherwise it starts it (reusing the session id if
    /// one was assigned before a failed start).
    ///
    /// Pay-per-use work that no longer fits in the month's cap is moved to
    /// Needs you on the way, and the next thread is tried, so capped work never
    /// holds up work on a Max account.
    ///
    /// A runner only gets work placed on its own kind of computer.
    pub async fn claim(&self, runner: &str, place: Place) -> Result<ClaimOutcome> {
        let actor = runner_actor(runner)?;
        // IMMEDIATE takes the write lock up front, so a claim never fails
        // halfway because an event was being written at the same moment.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let mut capped = Vec::new();
        loop {
            // Work on a Max account that is out of usage waits for the reset,
            // unless it (or its app) runs on pay-per-use. A runner only gets
            // work placed on its own kind of computer.
            let Some(row) = sqlx::query(
                "SELECT t.id, t.app_id, t.title, t.prompt, t.state, t.session_id, t.branch,
                        t.place, t.updated_at, t.parent_id, t.background, a.profile, (a.pay_per_use = 1 OR t.pay_per_use = 1) AS ppu
                 FROM threads t JOIN apps a ON a.id = t.app_id
                 WHERE t.state = 'queued' AND t.place = ?
                   AND (t.pay_per_use = 1 OR a.pay_per_use = 1 OR NOT EXISTS (
                        SELECT 1 FROM profile_limits l WHERE l.profile = a.profile
                            AND l.until > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')))
                 ORDER BY t.id LIMIT 1",
            )
            .bind(place.as_str())
            .fetch_optional(&mut *tx)
            .await?
            else {
                tx.commit().await?;
                return Ok(ClaimOutcome { work: None, capped });
            };
            let t = thread_from(&row)?;
            let profile: String = row.try_get("profile")?;
            let pay_per_use: bool = row.try_get("ppu")?;
            let account = if pay_per_use {
                "api".to_owned()
            } else {
                profile
            };
            let started: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM events WHERE thread_id = ?
                         AND json_extract(body, '$.kind') IN ('message', 'tool'))",
            )
            .bind(t.id)
            .fetch_one(&mut *tx)
            .await?;
            let resume = t.session_id.is_some() && started;
            let prior = if resume {
                crate::spend::session_cost_micros(&mut tx, t.id).await?
            } else {
                0
            };
            let Some(reserved) = crate::spend::open_run(&mut tx, t.id, &account, prior).await?
            else {
                // The month's pay-per-use money is used up: a hard stop for the owner.
                let cap = crate::spend::to_usd(crate::spend::cap_micros(&mut tx).await?);
                let done = sqlx::query(
                    "UPDATE threads SET state = 'needs_you',
                         updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                     WHERE id = ? AND state = 'queued'",
                )
                .bind(t.id)
                .execute(&mut *tx)
                .await?;
                if done.rows_affected() != 1 {
                    return Err(StoreError::Conflict);
                }
                let note = format!("monthly pay-per-use cap of ${cap:.2} reached");
                audit(
                    &mut tx,
                    t.id,
                    &actor,
                    ThreadState::Queued,
                    ThreadState::NeedsYou,
                    Some(&note),
                )
                .await?;
                capped.push(t);
                continue;
            };
            let session = t
                .session_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let branch = t
                .branch
                .clone()
                .unwrap_or_else(|| format!("decurion/thread-{}", t.id));
            let done = sqlx::query(
                "UPDATE threads SET state = 'running', runner = ?, session_id = ?, branch = ?,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE id = ? AND state = 'queued'",
            )
            .bind(&actor)
            .bind(&session)
            .bind(&branch)
            .bind(t.id)
            .execute(&mut *tx)
            .await?;
            if done.rows_affected() != 1 {
                return Err(StoreError::Conflict);
            }
            let note = if resume { "resumed" } else { "started" };
            audit(
                &mut tx,
                t.id,
                &actor,
                ThreadState::Queued,
                ThreadState::Running,
                Some(note),
            )
            .await?;
            let interrupted = if resume {
                interrupted_step(&mut tx, t.id).await?
            } else {
                None
            };
            let messages = crate::messages::list(&mut tx, t.id, true).await?;
            tx.commit().await?;
            let thread = self.thread(t.id).await?;
            let app = self.app(thread.app_id).await?;
            let checkpoint = self.latest_checkpoint(t.id).await?;
            let test_keys = self.test_key_names(app.id).await?;
            return Ok(ClaimOutcome {
                work: Some(Claim {
                    thread,
                    app,
                    resume,
                    checkpoint,
                    test_keys,
                    budget_usd: pay_per_use
                        .then(|| crate::spend::claude_budget_usd(prior, reserved)),
                    messages,
                    interrupted,
                    reserved_micros: pay_per_use.then_some(reserved),
                    prior_micros: prior,
                }),
                capped,
            });
        }
    }

    /// Called when a runner starts. Anything it was running when it died goes
    /// back to the queue with its session kept, so it resumes instead of
    /// starting over.
    pub async fn pause_runner_threads(&self, runner: &str) -> Result<Vec<i64>> {
        let actor = runner_actor(runner)?;
        let ids: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM threads WHERE state = 'running' AND runner = ?")
                .bind(&actor)
                .fetch_all(&self.pool)
                .await?;
        for id in &ids {
            self.move_thread(
                *id,
                Some(ThreadState::Running),
                ThreadState::Queued,
                &actor,
                Some("runner restarted; will resume"),
            )
            .await?;
        }
        Ok(ids)
    }

    /// Saves a run event. Only the runner the work is running on may report
    /// it (`runner` is its name): if the owner stopped or moved the work after
    /// the claim, the runner must not launch a box for it, unmetered, or
    /// report on it. Returns the event's id, and whether it started a new Max
    /// outage (which is worth a phone alert).
    pub async fn add_event(
        &self,
        thread_id: i64,
        runner: &str,
        e: &RunEvent,
    ) -> Result<(i64, bool)> {
        self.thread(thread_id).await?;
        let mut tx = self.pool.begin().await?;
        let holder: Option<String> = sqlx::query_scalar("SELECT runner FROM threads WHERE id = ?")
            .bind(thread_id)
            .fetch_one(&mut *tx)
            .await?;
        if state_in(&mut tx, thread_id).await? != ThreadState::Running
            || holder.as_deref() != Some(runner_actor(runner)?.as_str())
        {
            return Err(StoreError::Conflict);
        }
        let id = insert_event(&mut tx, thread_id, e).await?;
        let mut new_outage = false;
        if let RunEvent::LimitHit { reset_epoch } = e {
            // Only a Max run's limit counts; anything else is ignored.
            let Some(fresh) = crate::limits::record_limit(&mut tx, thread_id, *reset_epoch).await?
            else {
                tx.commit().await?;
                return Ok((id, false));
            };
            new_outage = fresh;
            // Record the outage and put the work back in line in one step, so
            // it can never restart before the wait is saved, and the alert
            // already counts it as waiting.
            let requeued = sqlx::query(
                "UPDATE threads SET state = 'queued',
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE id = ? AND state = 'running'",
            )
            .bind(thread_id)
            .execute(&mut *tx)
            .await?;
            if requeued.rows_affected() == 1 {
                crate::spend::close_open_run(&mut tx, thread_id).await?;
                audit(
                    &mut tx,
                    thread_id,
                    &runner_actor(runner)?,
                    ThreadState::Running,
                    ThreadState::Queued,
                    Some("Max account out of usage; waits for the reset"),
                )
                .await?;
            }
        }
        if let RunEvent::Finished {
            cost_usd: Some(cost),
            input_tokens,
            output_tokens,
            ..
        } = e
        {
            crate::spend::record_cost(&mut tx, thread_id, *cost, *input_tokens, *output_tokens)
                .await?;
        }
        tx.commit().await?;
        Ok((id, new_outage))
    }

    pub async fn events(&self, thread_id: i64, after: i64) -> Result<Vec<EventRecord>> {
        let rows = sqlx::query(
            "SELECT id, thread_id, at, body FROM events WHERE thread_id = ? AND id > ? ORDER BY id LIMIT 500",
        )
        .bind(thread_id)
        .bind(after)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let body: String = r.try_get("body")?;
                Ok(EventRecord {
                    id: r.try_get("id")?,
                    thread_id: r.try_get("thread_id")?,
                    at: r.try_get("at")?,
                    event: serde_json::from_str(&body)
                        .map_err(|e| StoreError::Invalid(e.to_string()))?,
                })
            })
            .collect()
    }

    /// The newest `n` events, oldest first.
    pub async fn events_tail(&self, thread_id: i64, n: i64) -> Result<Vec<EventRecord>> {
        let rows = sqlx::query(
            "SELECT * FROM (SELECT id, thread_id, at, body FROM events WHERE thread_id = ?
                 ORDER BY id DESC LIMIT ?) ORDER BY id",
        )
        .bind(thread_id)
        .bind(n)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let body: String = r.try_get("body")?;
                Ok(EventRecord {
                    id: r.try_get("id")?,
                    thread_id: r.try_get("thread_id")?,
                    at: r.try_get("at")?,
                    event: serde_json::from_str(&body)
                        .map_err(|e| StoreError::Invalid(e.to_string()))?,
                })
            })
            .collect()
    }

    pub async fn audit_count(&self, thread_id: i64) -> Result<i64> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM audit WHERE thread_id = ?")
                .bind(thread_id)
                .fetch_one(&self.pool)
                .await?,
        )
    }

    #[doc(hidden)]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// App names end up in folder names and docker mount arguments.
fn check_app_name(name: &str) -> Result<()> {
    let ok = (1..=40).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(StoreError::Invalid(
            "app name must be 1-40 letters, digits, - or _".into(),
        ))
    }
}

/// The repo path is handed to `git clone`: it must be absolute, so it cannot
/// be read as an option, and free of characters that break a docker mount.
fn check_repo_path(path: &str) -> Result<()> {
    let b = path.as_bytes();
    let windows_abs =
        b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
    let ok = (windows_abs || path.starts_with('/')) && !path.contains([',', '"', '\n', '\r']);
    if ok {
        Ok(())
    } else {
        Err(StoreError::Invalid(
            "repo_path must be an absolute path without commas or quotes".into(),
        ))
    }
}

/// Runner names are recorded as `runner:<name>`, so a runner can never write
/// an audit row that looks like the owner's.
pub(crate) fn runner_actor(name: &str) -> Result<String> {
    let ok = (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(format!("runner:{name}"))
    } else {
        Err(StoreError::Invalid("bad runner name".into()))
    }
}

/// One move inside the caller's transaction: allowed from the state the
/// thread is in now, conditional on that state so a racing move cannot also
/// succeed, and with its audit row. A thread leaving Running has its run
/// closed, so the money set aside for it is settled. Returns the state it
/// moved from.
pub(crate) async fn checked_move(
    tx: &mut Transaction<'_, Sqlite>,
    id: i64,
    expect: Option<ThreadState>,
    to: ThreadState,
    actor: &str,
    note: Option<&str>,
) -> Result<ThreadState> {
    let from = state_in(tx, id).await?;
    if expect.is_some_and(|e| e != from) {
        return Err(StoreError::Conflict);
    }
    if !from.can_move_to(to) {
        return Err(StoreError::BadMove { from, to });
    }
    let done = sqlx::query(
        "UPDATE threads SET state = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE id = ? AND state = ?",
    )
    .bind(to.as_str())
    .bind(id)
    .bind(from.as_str())
    .execute(&mut **tx)
    .await?;
    if done.rows_affected() != 1 {
        return Err(StoreError::Conflict);
    }
    if from == ThreadState::Running {
        crate::spend::close_open_run(tx, id).await?;
        // A move to pay-per-use during an outage is for that run only; a
        // pause (back to Queued) keeps it for the resumed run.
        if to != ThreadState::Queued {
            sqlx::query("UPDATE threads SET pay_per_use = 0 WHERE id = ?")
                .bind(id)
                .execute(&mut **tx)
                .await?;
        }
    }
    audit(tx, id, actor, from, to, note).await?;
    Ok(from)
}

pub(crate) async fn state_in(tx: &mut Transaction<'_, Sqlite>, id: i64) -> Result<ThreadState> {
    let s: String = sqlx::query_scalar("SELECT state FROM threads WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(StoreError::NotFound)?;
    ThreadState::from_str(&s).map_err(|e| StoreError::Invalid(e.to_string()))
}

/// Stops every unfinished thread under `id`, deepest first, so no piece of
/// a stopped task is left going without the task it belongs to. A
/// background thread and everything under it keeps going.
async fn stop_children(tx: &mut Transaction<'_, Sqlite>, id: i64) -> Result<()> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "WITH RECURSIVE down (id, depth) AS (
             SELECT id, 1 FROM threads WHERE parent_id = ? AND background = 0
             UNION ALL
             SELECT t.id, down.depth + 1 FROM threads t JOIN down ON t.parent_id = down.id
             WHERE t.background = 0
         )
         SELECT down.id FROM down JOIN threads t ON t.id = down.id
         WHERE t.state NOT IN ('done', 'failed')
         ORDER BY down.depth DESC, down.id",
    )
    .bind(id)
    .fetch_all(&mut **tx)
    .await?;
    for child in ids {
        checked_move(
            tx,
            child,
            None,
            ThreadState::Failed,
            OWNER,
            Some("stopped because the task it is part of was stopped"),
        )
        .await?;
    }
    Ok(())
}

/// New work can go under a task that is still open, in the same app, and
/// not already [`MAX_DEPTH`] deep.
async fn check_parent(tx: &mut Transaction<'_, Sqlite>, parent: i64, app_id: i64) -> Result<()> {
    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT app_id, state FROM threads WHERE id = ?")
            .bind(parent)
            .fetch_optional(&mut **tx)
            .await?;
    let Some((parent_app, state)) = row else {
        return Err(StoreError::Invalid(
            "the task this is part of does not exist".into(),
        ));
    };
    if parent_app != app_id {
        return Err(StoreError::Invalid(
            "work must be in the same app as the task it is part of".into(),
        ));
    }
    if matches!(state.as_str(), "done" | "failed") {
        return Err(StoreError::Refused(
            "the task this is part of is finished".into(),
        ));
    }
    // Counts the parent's ancestors, stopping once past the limit.
    let above: i64 = sqlx::query_scalar(
        "WITH RECURSIVE up (id, n) AS (
             SELECT parent_id, 1 FROM threads WHERE id = ? AND parent_id IS NOT NULL
             UNION ALL
             SELECT t.parent_id, up.n + 1 FROM threads t JOIN up ON t.id = up.id
             WHERE t.parent_id IS NOT NULL AND up.n <= ?
         )
         SELECT COALESCE(MAX(n), 0) FROM up",
    )
    .bind(parent)
    .bind(MAX_DEPTH)
    .fetch_one(&mut **tx)
    .await?;
    if above >= MAX_DEPTH {
        return Err(StoreError::Invalid(format!(
            "work can only be split {MAX_DEPTH} levels deep"
        )));
    }
    Ok(())
}

/// Where a resumed run was cut off: set when the agent's last event (the
/// owner's messages and box starts aside, since a box that started and died
/// again did nothing new) is a tool call it never got the answer to. The
/// log gets an Interrupted event, once; a later claim of the same cut-off
/// finds that event and reuses it.
async fn interrupted_step(
    tx: &mut Transaction<'_, Sqlite>,
    thread_id: i64,
) -> Result<Option<Interrupted>> {
    let body: Option<String> = sqlx::query_scalar(
        "SELECT body FROM events WHERE thread_id = ?
             AND json_extract(body, '$.kind') NOT IN ('owner', 'started')
         ORDER BY id DESC LIMIT 1",
    )
    .bind(thread_id)
    .fetch_optional(&mut **tx)
    .await?;
    // An event this hub cannot read is not proof of anything.
    let last = body.and_then(|b| serde_json::from_str::<RunEvent>(&b).ok());
    Ok(match last {
        Some(RunEvent::Tool { name, summary }) => {
            let step = Interrupted {
                safe_to_redo: decurion_proto::safe_to_redo(&name),
                name,
                summary,
            };
            let e = RunEvent::Interrupted {
                name: step.name.clone(),
                summary: step.summary.clone(),
                safe_to_redo: step.safe_to_redo,
            };
            insert_event(tx, thread_id, &e).await?;
            Some(step)
        }
        Some(RunEvent::Interrupted {
            name,
            summary,
            safe_to_redo,
        }) => Some(Interrupted {
            name,
            summary,
            safe_to_redo,
        }),
        _ => None,
    })
}

pub(crate) async fn insert_event(
    tx: &mut Transaction<'_, Sqlite>,
    thread_id: i64,
    e: &RunEvent,
) -> Result<i64> {
    let body = serde_json::to_string(e).map_err(|e| StoreError::Invalid(e.to_string()))?;
    Ok(
        sqlx::query_scalar("INSERT INTO events (thread_id, body) VALUES (?, ?) RETURNING id")
            .bind(thread_id)
            .bind(body)
            .fetch_one(&mut **tx)
            .await?,
    )
}

pub(crate) fn check_receipt(receipt: Option<&str>) -> Result<()> {
    match receipt {
        Some(r) if !decurion_proto::check_receipt(r) => Err(StoreError::Invalid(
            "receipt must be 8-64 letters, digits, - or _".into(),
        )),
        _ => Ok(()),
    }
}

/// What a receipt was used for before: the id it produced if it was used for
/// `action`, nothing if it is new. A receipt stands for one tap, so reusing
/// it for a different action is refused rather than guessed at.
pub(crate) async fn receipt_result(
    tx: &mut Transaction<'_, Sqlite>,
    receipt: Option<&str>,
    action: &str,
) -> Result<Option<i64>> {
    let Some(r) = receipt else {
        return Ok(None);
    };
    let row: Option<(String, i64)> =
        sqlx::query_as("SELECT action, result_id FROM receipts WHERE receipt = ?")
            .bind(r)
            .fetch_optional(&mut **tx)
            .await?;
    match row {
        None => Ok(None),
        Some((a, id)) if a == action => Ok(Some(id)),
        Some(_) => Err(StoreError::Duplicate(RECEIPT_REUSED.into())),
    }
}

/// Written in the action's own transaction, so a receipt is stored exactly
/// when the action happened.
pub(crate) async fn save_receipt(
    tx: &mut Transaction<'_, Sqlite>,
    receipt: Option<&str>,
    action: &str,
    result_id: i64,
) -> Result<()> {
    if let Some(r) = receipt {
        sqlx::query("INSERT INTO receipts (receipt, action, result_id) VALUES (?, ?, ?)")
            .bind(r)
            .bind(action)
            .bind(result_id)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

pub(crate) async fn audit(
    tx: &mut Transaction<'_, Sqlite>,
    thread_id: i64,
    actor: &str,
    from: ThreadState,
    to: ThreadState,
    note: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO audit (thread_id, actor, from_state, to_state, note) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(thread_id)
    .bind(actor)
    .bind(from.as_str())
    .bind(to.as_str())
    .bind(note)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
