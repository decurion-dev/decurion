//! SQLite-backed board state. Every state change goes through [`Store::move_thread`]
//! (or the claim/pause paths), which check the allowed moves and write an audit row
//! in the same transaction.

use decurion_proto::{
    App, Claim, EventRecord, NewApp, NewThread, Profile, RunEvent, Thread, ThreadState,
};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};
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
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

const THREAD_COLS: &str = "id, app_id, title, prompt, state, session_id, branch, updated_at";

fn thread_from(row: &SqliteRow) -> Result<Thread> {
    let state: String = row.try_get("state")?;
    Ok(Thread {
        id: row.try_get("id")?,
        app_id: row.try_get("app_id")?,
        title: row.try_get("title")?,
        prompt: row.try_get("prompt")?,
        state: ThreadState::from_str(&state).map_err(|e| StoreError::Invalid(e.to_string()))?,
        session_id: row.try_get("session_id")?,
        branch: row.try_get("branch")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn app_from(row: &SqliteRow) -> Result<App> {
    let profile: String = row.try_get("profile")?;
    Ok(App {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        repo_path: row.try_get("repo_path")?,
        profile: Profile::from_str(&profile).map_err(|e| StoreError::Invalid(e.to_string()))?,
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
            "INSERT INTO apps (name, repo_path, profile) VALUES (?, ?, ?)
             RETURNING id, name, repo_path, profile",
        )
        .bind(&a.name)
        .bind(&a.repo_path)
        .bind(a.profile.as_str())
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
        let rows = sqlx::query("SELECT id, name, repo_path, profile FROM apps ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(app_from).collect()
    }

    pub async fn app(&self, id: i64) -> Result<App> {
        let row = sqlx::query("SELECT id, name, repo_path, profile FROM apps WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)?;
        app_from(&row)
    }

    pub async fn add_thread(&self, t: &NewThread) -> Result<Thread> {
        if t.title.trim().is_empty() || t.prompt.trim().is_empty() {
            return Err(StoreError::Invalid("title and prompt are required".into()));
        }
        self.app(t.app_id).await?;
        let row = sqlx::query(&format!(
            "INSERT INTO threads (app_id, title, prompt) VALUES (?, ?, ?) RETURNING {THREAD_COLS}"
        ))
        .bind(t.app_id)
        .bind(t.title.trim())
        .bind(t.prompt.trim())
        .fetch_one(&self.pool)
        .await?;
        thread_from(&row)
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
    /// cannot both succeed.
    pub async fn move_thread(
        &self,
        id: i64,
        expect: Option<ThreadState>,
        to: ThreadState,
        actor: &str,
        note: Option<&str>,
    ) -> Result<Thread> {
        let mut tx = self.pool.begin().await?;
        let from: String = sqlx::query_scalar("SELECT state FROM threads WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StoreError::NotFound)?;
        let from = ThreadState::from_str(&from).map_err(|e| StoreError::Invalid(e.to_string()))?;
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
        .execute(&mut *tx)
        .await?;
        if done.rows_affected() != 1 {
            return Err(StoreError::Conflict);
        }
        audit(&mut tx, id, actor, from, to, note).await?;
        tx.commit().await?;
        self.thread(id).await
    }

    /// Hands the oldest queued thread to `runner` and marks it running. A thread
    /// whose agent already produced activity was paused mid-run, so the runner
    /// resumes that session; otherwise it starts it (reusing the session id if
    /// one was assigned before a failed start).
    pub async fn claim(&self, runner: &str) -> Result<Option<Claim>> {
        let actor = runner_actor(runner)?;
        let mut tx = self.pool.begin().await?;
        let Some(row) = sqlx::query(&format!(
            "SELECT {THREAD_COLS} FROM threads WHERE state = 'queued' ORDER BY id LIMIT 1"
        ))
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(None);
        };
        let t = thread_from(&row)?;
        let started: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM events WHERE thread_id = ?
                     AND json_extract(body, '$.kind') IN ('message', 'tool'))",
        )
        .bind(t.id)
        .fetch_one(&mut *tx)
        .await?;
        let resume = t.session_id.is_some() && started;
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
        tx.commit().await?;
        let thread = self.thread(t.id).await?;
        let app = self.app(thread.app_id).await?;
        Ok(Some(Claim {
            thread,
            app,
            resume,
        }))
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

    pub async fn add_event(&self, thread_id: i64, e: &RunEvent) -> Result<i64> {
        self.thread(thread_id).await?;
        let body = serde_json::to_string(e).map_err(|e| StoreError::Invalid(e.to_string()))?;
        let id =
            sqlx::query_scalar("INSERT INTO events (thread_id, body) VALUES (?, ?) RETURNING id")
                .bind(thread_id)
                .bind(body)
                .fetch_one(&self.pool)
                .await?;
        Ok(id)
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
fn runner_actor(name: &str) -> Result<String> {
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

async fn audit(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
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
