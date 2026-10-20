//! Where work runs: the PC runner and cloud workers, and moving work between
//! them.
//!
//! The PC runner signs in with the runner key in `keys.json` and is always
//! called `local`. A cloud worker signs up once with a one-time code from its
//! setup text and gets its own key, kept here only as a hash. Removing a cloud
//! worker revokes its key and brings its work back to the PC.

use crate::devices::hash;
use crate::store::{audit, runner_actor, Result, Store, StoreError};
use decurion_proto::{Place, Thread, ThreadState};
use serde::Serialize;
use sqlx::Row;
use std::str::FromStr;

/// The PC runner's name.
pub const LOCAL: &str = "local";

/// How long the code in a cloud worker's setup text works. Long enough to
/// rent a server and let it build Decurion.
pub const JOIN_HOURS: i64 = 24;

pub const NO_CLOUD_WORKER: &str =
    "There is no cloud worker yet. Add one first: Settings, Add cloud worker.";

const HAS_CLOUD_WORKER: &str = "SELECT EXISTS (SELECT 1 FROM runners WHERE removed_at IS NULL)";

/// Whether a cloud worker is set up, read inside the caller's transaction.
pub(crate) async fn cloud_worker_in(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<bool> {
    Ok(sqlx::query_scalar(HAS_CLOUD_WORKER)
        .fetch_one(&mut **tx)
        .await?)
}

/// A computer that runs work, as the board shows it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Worker {
    /// Set for cloud workers, which can be removed.
    pub id: Option<i64>,
    pub name: String,
    pub place: Place,
    pub online: bool,
    /// Seconds since the hub last heard from it; `None` if not since the hub
    /// started.
    pub seconds_ago: Option<u64>,
}

/// A runner, as the hub knows it from its key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerId {
    pub name: String,
    pub place: Place,
}

impl RunnerId {
    pub fn local() -> Self {
        Self {
            name: LOCAL.into(),
            place: Place::Pc,
        }
    }

    /// How its moves and audit rows are recorded.
    pub fn actor(&self) -> String {
        format!("runner:{}", self.name)
    }
}

impl Store {
    /// A new one-time code for a cloud worker's setup text, and the name the
    /// worker will get.
    pub async fn new_join_code(&self) -> Result<(String, String)> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let seq: String =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = 'cloud_worker_seq'")
                .fetch_one(&mut *tx)
                .await?;
        let n = seq.parse::<i64>().unwrap_or(0) + 1;
        sqlx::query("UPDATE settings SET value = ? WHERE key = 'cloud_worker_seq'")
            .bind(n.to_string())
            .execute(&mut *tx)
            .await?;
        let name = format!("cloud-{n}");
        let code = crate::new_secret();
        sqlx::query(
            "INSERT INTO runner_join_codes (code_hash, name, expires_at)
             VALUES (?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?))",
        )
        .bind(hash(&code))
        .bind(&name)
        .bind(format!("+{JOIN_HOURS} hours"))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((code, name))
    }

    /// Swaps a setup code for the worker's own key. Works once, before the
    /// code expires; returns the worker's name and key.
    pub async fn join_cloud_worker(&self, code: &str) -> Result<Option<(String, String)>> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let name: Option<String> = sqlx::query_scalar(
            "UPDATE runner_join_codes SET used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE code_hash = ? AND used_at IS NULL
               AND expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             RETURNING name",
        )
        .bind(hash(code))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(name) = name else {
            return Ok(None);
        };
        let token = crate::new_secret();
        sqlx::query("INSERT INTO runners (name, token_hash) VALUES (?, ?)")
            .bind(&name)
            .bind(hash(&token))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some((name, token)))
    }

    /// The cloud worker a key belongs to, unless it was removed.
    pub async fn cloud_runner_for_token(&self, token: &str) -> Result<Option<RunnerId>> {
        let name: Option<String> = sqlx::query_scalar(
            "SELECT name FROM runners WHERE token_hash = ? AND removed_at IS NULL",
        )
        .bind(hash(token))
        .fetch_optional(self.pool())
        .await?;
        Ok(name.map(|name| RunnerId {
            name,
            place: Place::Cloud,
        }))
    }

    /// Cloud workers that have not been removed: `(id, name)`.
    pub async fn cloud_workers(&self) -> Result<Vec<(i64, String)>> {
        let rows = sqlx::query("SELECT id, name FROM runners WHERE removed_at IS NULL ORDER BY id")
            .fetch_all(self.pool())
            .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("id")?, r.try_get("name")?)))
            .collect()
    }

    pub async fn has_cloud_worker(&self) -> Result<bool> {
        Ok(sqlx::query_scalar(HAS_CLOUD_WORKER)
            .fetch_one(self.pool())
            .await?)
    }

    /// Moves work to the PC or the cloud. Running work keeps running until
    /// its runner notices and hands it over with a saved copy; anything else
    /// is simply picked up by the other computer next time it runs.
    pub async fn set_place(&self, id: i64, place: Place, actor: &str) -> Result<Thread> {
        // Bring up to date runs get the base branch in their starting copy
        // from this PC's hub; they stay here.
        if place == Place::Cloud && self.merge_from(id).await?.is_some() {
            return Err(StoreError::Refused(
                "Bringing a pull request up to date runs on this PC only, so it stays here.".into(),
            ));
        }
        if place == Place::Cloud && !self.has_cloud_worker().await? {
            return Err(StoreError::Refused(NO_CLOUD_WORKER.into()));
        }
        // Work that ran before saved copies existed has only its PC folder;
        // the other computer would start it over on an old branch. Running
        // work is fine: its runner saves a copy as it hands it over.
        let unsaved: bool = sqlx::query_scalar(
            "SELECT branch IS NOT NULL AND state NOT IN ('running', 'done')
                 AND NOT EXISTS (SELECT 1 FROM checkpoints c WHERE c.thread_id = threads.id)
             FROM threads WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(StoreError::NotFound)?;
        if unsaved {
            return Err(StoreError::Refused(
                "This work started before it could move between computers. Finish or retry it where it is.".into(),
            ));
        }
        self.place_unchecked(id, place, actor).await
    }

    /// Moves work without the saved-copy check, for when it has to move
    /// anyway because its computer is gone.
    async fn place_unchecked(&self, id: i64, place: Place, actor: &str) -> Result<Thread> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT state, place FROM threads WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let (state, from) = row.ok_or(StoreError::NotFound)?;
        let state =
            ThreadState::from_str(&state).map_err(|e| StoreError::Invalid(e.to_string()))?;
        if state == ThreadState::Done {
            return Err(StoreError::Refused(
                "Finished work stays where it is.".into(),
            ));
        }
        if from != place.as_str() {
            sqlx::query("UPDATE threads SET place = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?")
                .bind(place.as_str())
                .bind(id)
                .execute(&mut *tx)
                .await?;
            let note = match place {
                Place::Pc => "moved to the PC",
                Place::Cloud => "moved to the cloud",
            };
            audit(&mut tx, id, actor, state, state, Some(note)).await?;
        }
        tx.commit().await?;
        self.thread(id).await
    }

    /// Revokes a cloud worker's key. Work it was running goes back to the
    /// queue, and if no cloud worker is left, all cloud work moves to the PC
    /// so nothing waits for a computer that is gone.
    pub async fn remove_cloud_worker(&self, id: i64) -> Result<()> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let name: Option<String> = sqlx::query_scalar(
            "UPDATE runners SET removed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ? AND removed_at IS NULL RETURNING name",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let name = name.ok_or(StoreError::NotFound)?;
        tx.commit().await?;
        let actor = runner_actor(&name)?;
        let running: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM threads WHERE state = 'running' AND runner = ?")
                .bind(&actor)
                .fetch_all(self.pool())
                .await?;
        for t in running {
            self.move_thread(
                t,
                Some(ThreadState::Running),
                ThreadState::Queued,
                &actor,
                Some("cloud worker removed; will continue from its last saved copy"),
            )
            .await?;
        }
        if !self.has_cloud_worker().await? {
            let cloud: Vec<i64> = sqlx::query_scalar(
                "SELECT id FROM threads WHERE place = 'cloud' AND state != 'done'",
            )
            .fetch_all(self.pool())
            .await?;
            for t in cloud {
                self.place_unchecked(t, Place::Pc, "owner").await?;
            }
        }
        Ok(())
    }
}
