//! SQLite-backed board state. Every state change goes through [`checked_move`]
//! (or the claim path), which checks the allowed moves and writes an audit row
//! in the same transaction.

use decurion_proto::{
    deploy, sites, Account, Ai, App, AppRules, Claim, EventRecord, Interrupted, NewApp, NewThread,
    Place, Profile, RunEvent, Thread, ThreadState,
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

/// The owner's changes to an app ([`Store::edit_app`]). A field left out
/// stays as it is.
#[derive(Debug, Default, serde::Deserialize)]
pub struct AppEdit {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub repo_path: Option<String>,
    #[serde(default)]
    pub profile: Option<Profile>,
    /// Left out: unchanged. `null` or empty: no one. A GitHub login: them.
    #[serde(default, deserialize_with = "present")]
    pub reviewer: Option<Option<String>>,
    #[serde(default)]
    pub receipt: Option<String>,
}

/// The owner's changes to an app's rules ([`Store::set_rules`]). A field
/// left out stays as it is.
#[derive(Debug, Default, serde::Deserialize)]
pub struct RulesEdit {
    #[serde(default)]
    pub merge: Option<bool>,
    #[serde(default)]
    pub deploy: Option<bool>,
    #[serde(default)]
    pub deploy_steps: Option<String>,
    /// Left out: unchanged. `null` or empty: none. An https address: it.
    #[serde(default, deserialize_with = "present")]
    pub deploy_site: Option<Option<String>>,
    #[serde(default)]
    pub send_checked: Option<bool>,
    #[serde(default)]
    pub patient_data: Option<bool>,
    #[serde(default)]
    pub receipt: Option<String>,
}

/// Tells a field sent as `null` (`Some(None)`) from one left out (`None`).
fn present<'de, D, T>(d: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    T::deserialize(d).map(Some)
}

const THREAD_COLS: &str =
    "id, app_id, title, prompt, state, ai, session_id, branch, place, updated_at, parent_id, background,
     gh_state, gh_branch, gh_url, gh_pr, gh_error, fix_of, research, dismissed_at";

/// The actor recorded for the owner's own moves.
pub const OWNER: &str = "owner";

/// A task with this many tasks above it cannot be split further. Deep trees
/// are hard to follow on a phone and usually mean an agent is looping.
pub const MAX_DEPTH: i64 = 5;

pub(crate) const RECEIPT_REUSED: &str = "this receipt was already used for something else";

pub(crate) const APP_COLS: &str =
    "id, name, repo_path, profile, pay_per_use, monthly_cap_micros, sites, reviewer,
     COALESCE(first_on = date('now', 'localtime'), 0) AS first_today,
     can_merge, can_deploy, deploy_steps, deploy_site, send_checked, patient_data";

fn thread_from(row: &SqliteRow) -> Result<Thread> {
    let state: String = row.try_get("state")?;
    let ai: String = row.try_get("ai")?;
    let place: String = row.try_get("place")?;
    Ok(Thread {
        id: row.try_get("id")?,
        app_id: row.try_get("app_id")?,
        title: row.try_get("title")?,
        prompt: row.try_get("prompt")?,
        state: ThreadState::from_str(&state).map_err(|e| StoreError::Invalid(e.to_string()))?,
        ai: Ai::from_str(&ai).map_err(|e| StoreError::Invalid(e.to_string()))?,
        session_id: row.try_get("session_id")?,
        branch: row.try_get("branch")?,
        place: Place::from_str(&place).map_err(|e| StoreError::Invalid(e.to_string()))?,
        updated_at: row.try_get("updated_at")?,
        parent_id: row.try_get("parent_id")?,
        background: row.try_get("background")?,
        github: crate::github::send_from(row)?,
        fix_of: row.try_get("fix_of")?,
        research: row.try_get("research")?,
        dismissed_at: row.try_get("dismissed_at")?,
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
        sites: serde_json::from_str(row.try_get("sites")?)
            .map_err(|e| StoreError::Invalid(e.to_string()))?,
        monthly_cap_usd: row
            .try_get::<Option<i64>, _>("monthly_cap_micros")?
            .map(crate::spend::to_usd),
        reviewer: row.try_get("reviewer")?,
        first_today: row.try_get("first_today")?,
        rules: AppRules {
            merge: row.try_get("can_merge")?,
            deploy: row.try_get("can_deploy")?,
            deploy_steps: row.try_get("deploy_steps")?,
            deploy_site: row.try_get("deploy_site")?,
            send_checked: row.try_get("send_checked")?,
            patient_data: row.try_get("patient_data")?,
        },
    })
}

impl Store {
    /// Opens (creating if needed) the database at `url`, e.g. `sqlite://hub.db`
    /// or `sqlite::memory:`, and applies migrations.
    pub async fn open(url: &str) -> Result<Self> {
        // A writer waits up to this long for another writer. That only works
        // for transactions that take the write lock when they start (`BEGIN
        // IMMEDIATE`): one that reads first and writes later is refused at
        // once instead of waiting, so every transaction here starts that way.
        let opts = SqliteConnectOptions::from_str(url)?
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(10))
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
        let sites = clean_sites(&a.sites)?;
        let row = sqlx::query(&format!(
            "INSERT INTO apps (name, repo_path, profile, pay_per_use, sites) VALUES (?, ?, ?, ?, ?)
             RETURNING {APP_COLS}"
        ))
        .bind(&a.name)
        .bind(&a.repo_path)
        .bind(a.profile.as_str())
        .bind(a.pay_per_use)
        .bind(sites_json(&sites)?)
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
        let rows = sqlx::query(&format!("SELECT {APP_COLS} FROM apps ORDER BY name"))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(app_from).collect()
    }

    pub async fn app(&self, id: i64) -> Result<App> {
        let row = sqlx::query(&format!("SELECT {APP_COLS} FROM apps WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)?;
        app_from(&row)
    }

    async fn app_in(&self, tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, id: i64) -> Result<App> {
        let row = sqlx::query(&format!("SELECT {APP_COLS} FROM apps WHERE id = ?"))
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or(StoreError::NotFound)?;
        app_from(&row)
    }

    /// Replaces the websites an app's agents may reach on top of the built-in
    /// ones, and returns the list as stored. Running work keeps the list it
    /// started with; the next piece of work gets this one.
    pub async fn set_sites(&self, app_id: i64, sites: &[String]) -> Result<Vec<String>> {
        let sites = clean_sites(sites)?;
        let done = sqlx::query("UPDATE apps SET sites = ? WHERE id = ?")
            .bind(sites_json(&sites)?)
            .bind(app_id)
            .execute(&self.pool)
            .await?;
        if done.rows_affected() != 1 {
            return Err(StoreError::NotFound);
        }
        Ok(sites)
    }

    /// Changes an app the owner already added: its name, its folder, its Max
    /// plan or who reviews its pull requests. Fields left out stay as they
    /// are. This owner tap is the only way an app's Max plan ever changes;
    /// nothing switches plans by itself (PLAN.md D1, D2). Work already
    /// running keeps the plan it started with, and the next run uses the new
    /// one.
    ///
    /// The runner keeps each piece of work's files in folders named after
    /// the app, so a new name or folder waits until none of the app's work
    /// is running or paused partway (queued or with the owner after its agent
    /// already started), which would pick up from those files. Work in
    /// review or finished is safe: its saved copy is on the hub.
    ///
    /// With a receipt already used for this, nothing changes and the app is
    /// returned as it is now; the flag is false then.
    pub async fn edit_app(&self, app_id: i64, e: &AppEdit) -> Result<(App, bool)> {
        check_receipt(e.receipt.as_deref())?;
        if let Some(n) = &e.name {
            check_app_name(n)?;
        }
        if let Some(p) = &e.repo_path {
            check_repo_path(p)?;
        }
        let reviewer = match &e.reviewer {
            None => None,
            Some(r) => Some(clean_reviewer(r.as_deref())?),
        };
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(done) = receipt_result(&mut tx, e.receipt.as_deref(), "app_edit").await? {
            if done != app_id {
                return Err(StoreError::Duplicate(RECEIPT_REUSED.into()));
            }
            tx.commit().await?;
            return Ok((self.app(app_id).await?, false));
        }
        let app = self.app_in(&mut tx, app_id).await?;
        let name = e.name.as_ref().filter(|n| **n != app.name);
        let folder = e.repo_path.as_ref().filter(|p| **p != app.repo_path);
        if (name.is_some() || folder.is_some()) && work_in_progress(&mut tx, app_id).await? {
            let what = if name.is_some() {
                "rename it. Work that is running or paused partway keeps its files under the app's name."
            } else {
                "change its folder. Work that is running or paused partway still needs the folder it started from."
            };
            return Err(StoreError::Refused(format!(
                "Wait until this app's running work finishes, then {what}"
            )));
        }
        if let Some(n) = name {
            // Folders on Windows ignore capitals, so two apps may not differ
            // only by them.
            let taken: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM apps WHERE id <> ? AND lower(name) = lower(?))",
            )
            .bind(app_id)
            .bind(n)
            .fetch_one(&mut *tx)
            .await?;
            if taken {
                return Err(StoreError::Duplicate(taken_words(n)));
            }
        }
        sqlx::query(
            "UPDATE apps SET name = COALESCE(?, name), repo_path = COALESCE(?, repo_path),
                 profile = COALESCE(?, profile),
                 reviewer = CASE WHEN ? THEN ? ELSE reviewer END
             WHERE id = ?",
        )
        .bind(name)
        .bind(folder)
        .bind(e.profile.map(Profile::as_str))
        .bind(reviewer.is_some())
        .bind(reviewer.flatten())
        .bind(app_id)
        .execute(&mut *tx)
        .await
        .map_err(|err| match err.as_database_error() {
            Some(d) if d.is_unique_violation() => {
                StoreError::Duplicate(taken_words(name.map_or("", String::as_str)))
            }
            _ => StoreError::Db(err),
        })?;
        save_receipt(&mut tx, e.receipt.as_deref(), "app_edit", app_id).await?;
        let app = self.app_in(&mut tx, app_id).await?;
        tx.commit().await?;
        Ok((app, true))
    }

    /// Changes an app's rules, as the owner set them on its Settings page.
    /// Fields left out stay as they are. Deploy steps and the live site are
    /// checked whenever they are given, and deploying cannot be on without
    /// steps that read cleanly, so a Deploy button never runs something the
    /// owner did not plainly write. Only the owner reaches this (owner-only
    /// route); nothing in an app's repo or an agent's answer can set it.
    ///
    /// With a receipt already used for this, nothing changes and the app is
    /// returned as it is now; the flag is false then.
    pub async fn set_rules(&self, app_id: i64, e: &RulesEdit) -> Result<(App, bool)> {
        check_receipt(e.receipt.as_deref())?;
        let steps = e.deploy_steps.as_deref().map(clean_steps).transpose()?;
        let site = match &e.deploy_site {
            None => None,
            Some(s) => Some(
                match s.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    None => None,
                    Some(s) => Some(deploy::check_site(s).map_err(StoreError::Invalid)?),
                },
            ),
        };
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(done) = receipt_result(&mut tx, e.receipt.as_deref(), "app_rules").await? {
            if done != app_id {
                return Err(StoreError::Duplicate(RECEIPT_REUSED.into()));
            }
            tx.commit().await?;
            return Ok((self.app(app_id).await?, false));
        }
        let now = self.app_in(&mut tx, app_id).await?.rules;
        let rules = AppRules {
            merge: e.merge.unwrap_or(now.merge),
            deploy: e.deploy.unwrap_or(now.deploy),
            deploy_steps: steps.unwrap_or(now.deploy_steps),
            deploy_site: site.unwrap_or(now.deploy_site),
            send_checked: e.send_checked.unwrap_or(now.send_checked),
            patient_data: e.patient_data.unwrap_or(now.patient_data),
        };
        if rules.deploy && rules.deploy_steps().is_none() {
            return Err(StoreError::Invalid(
                "Add the deploy steps first, one command per line, then turn on Deploy from Decurion."
                    .into(),
            ));
        }
        sqlx::query(
            "UPDATE apps SET can_merge = ?, can_deploy = ?, deploy_steps = ?, deploy_site = ?,
                 send_checked = ?, patient_data = ?
             WHERE id = ?",
        )
        .bind(rules.merge)
        .bind(rules.deploy)
        .bind(&rules.deploy_steps)
        .bind(&rules.deploy_site)
        .bind(rules.send_checked)
        .bind(rules.patient_data)
        .bind(app_id)
        .execute(&mut *tx)
        .await?;
        save_receipt(&mut tx, e.receipt.as_deref(), "app_rules", app_id).await?;
        let app = self.app_in(&mut tx, app_id).await?;
        tx.commit().await?;
        Ok((app, true))
    }

    /// Stars an app to go first in the queue today, or takes the star off.
    /// The star is the hub's local date, so it ends by itself when the day
    /// does. It only orders the queue: caps, Max limits and places still
    /// apply to a starred app's work as to any other.
    ///
    /// With a receipt already used for this, nothing changes and the app is
    /// returned as it is now; the flag is false then.
    pub async fn set_first_today(
        &self,
        app_id: i64,
        on: bool,
        receipt: Option<&str>,
    ) -> Result<(App, bool)> {
        check_receipt(receipt)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(done) = receipt_result(&mut tx, receipt, "app_first").await? {
            if done != app_id {
                return Err(StoreError::Duplicate(RECEIPT_REUSED.into()));
            }
            tx.commit().await?;
            return Ok((self.app(app_id).await?, false));
        }
        let done = sqlx::query(
            "UPDATE apps SET first_on = CASE WHEN ? THEN date('now', 'localtime') END WHERE id = ?",
        )
        .bind(on)
        .bind(app_id)
        .execute(&mut *tx)
        .await?;
        if done.rows_affected() != 1 {
            return Err(StoreError::NotFound);
        }
        save_receipt(&mut tx, receipt, "app_first", app_id).await?;
        let app = self.app_in(&mut tx, app_id).await?;
        tx.commit().await?;
        Ok((app, true))
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
            "INSERT INTO threads (app_id, title, prompt, parent_id, background, place, ai)
             VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING {THREAD_COLS}"
        ))
        .bind(t.app_id)
        .bind(t.title.trim())
        .bind(t.prompt.trim())
        .bind(t.parent_id)
        .bind(t.background)
        .bind(t.place.as_str())
        .bind(t.ai.as_str())
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

    /// [`Store::move_thread`] as asked for from the board or the runner. Three
    /// moves do more than change the state:
    ///
    /// - The owner stopping work (to Failed) first stops the work under it,
    ///   deepest first, in the same transaction, so nothing is left running
    ///   for a task that was called off. Background work is left alone.
    /// - The runner finishing a run (Running to In review) while the owner
    ///   has a steer or follow-up waiting sends it back to the queue instead,
    ///   so the agent gets them before anyone reviews it.
    /// - The owner accepting work in review (to Done) records, in the same
    ///   transaction, that the hub is to send it to GitHub
    ///   ([`crate::github`]). The sending itself happens after, and a
    ///   failure there never undoes the accept.
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
        // Only the runner says work is finished: on the way to review it checks
        // the branch for test keys, and accepted work then goes to GitHub. An
        // owner's tap must not skip that check.
        if actor == OWNER && from == ThreadState::Running && to == ThreadState::InReview {
            return Err(StoreError::Refused(
                "Work goes to review when its agent finishes. To end it now, stop it instead."
                    .into(),
            ));
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
        // The owner accepting reviewed work: the wish to send it to GitHub
        // is saved with the accept, so a crash right after still sends it.
        if actor == OWNER && from == ThreadState::InReview && to == ThreadState::Done {
            crate::github::want_send(&mut tx, id).await?;
        }
        save_receipt(&mut tx, receipt, "move", id).await?;
        tx.commit().await?;
        Ok((self.thread(id).await?, true))
    }

    /// Hands the next queued thread to `runner` and marks it running. A thread
    /// whose agent already produced activity was paused mid-run, so the runner
    /// resumes that session; otherwise it starts it (reusing the session id if
    /// one was assigned before a failed start).
    ///
    /// Apps take turns, so one big goal never holds up every other app: apps
    /// the owner starred today come first, then the app whose work was handed
    /// out longest ago (never, first), then that app's oldest work. Handing
    /// out work stamps its app in the same transaction; capped work that
    /// moves to Needs you does not, since nothing ran.
    ///
    /// Pay-per-use work that no longer fits in the month's cap is moved to
    /// Needs you on the way, and the next thread is tried, so capped work never
    /// holds up work on a Max account.
    ///
    /// A runner only gets work placed on its own kind of computer.
    /// `busy` is work the runner is still running or cleaning up after: it
    /// is never handed back to that runner until it leaves the list, so two
    /// attempts at one piece never overlap on one computer.
    pub async fn claim(&self, runner: &str, place: Place, busy: &[i64]) -> Result<ClaimOutcome> {
        let actor = runner_actor(runner)?;
        // IMMEDIATE takes the write lock up front, so a claim never fails
        // halfway because an event was being written at the same moment.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let mut capped = Vec::new();
        loop {
            // Work on a Max account that is out of usage waits for the reset,
            // unless it (or its app) runs on pay-per-use, the owner chose
            // pay-per-use until the reset (limits.rs), or another AI does it.
            // A runner only gets work placed on its own kind of computer.
            let Some(row) = sqlx::query(&format!(
                "SELECT t.id, t.app_id, t.title, t.prompt, t.state, t.ai, t.session_id, t.branch,
                        t.place, t.updated_at, t.parent_id, t.background, t.pay_per_use AS thread_ppu,
                        t.gh_state, t.gh_branch, t.gh_url, t.gh_pr, t.gh_error, t.fix_of, t.research, t.dismissed_at,
                        EXISTS (SELECT 1 FROM profile_limits l WHERE l.profile = a.profile
                            AND {ppu}) AS plan_ppu
                 FROM threads t JOIN apps a ON a.id = t.app_id
                 WHERE t.state = 'queued' AND t.place = ?
                   AND t.id NOT IN (SELECT value FROM json_each(?))
                   AND (t.ai <> 'claude' OR t.pay_per_use = 1 OR a.pay_per_use = 1 OR NOT EXISTS (
                        SELECT 1 FROM profile_limits l WHERE l.profile = a.profile
                            AND l.until > strftime('%Y-%m-%dT%H:%M:%fZ', 'now') AND NOT {ppu}))
                 ORDER BY COALESCE(a.first_on = date('now', 'localtime'), 0) DESC,
                          a.last_claim_at IS NOT NULL, a.last_claim_at, t.id
                 LIMIT 1",
                ppu = crate::limits::PAY_PER_USE_NOW
            ))
            .bind(place.as_str())
            .bind(serde_json::to_string(busy).expect("a list of ids is JSON"))
            .fetch_optional(&mut *tx)
            .await?
            else {
                tx.commit().await?;
                return Ok(ClaimOutcome { work: None, capped });
            };
            let t = thread_from(&row)?;
            let app = self.app_in(&mut tx, t.app_id).await?;
            // The owner chose pay-per-use while its Max account is out: for
            // the account until its reset, or (hubs before migration 0077)
            // for this one piece's next run. It is set aside and capped below
            // like any paid run; it never moves to the other Max account.
            let account = match Account::for_work(t.ai, &app) {
                Account::A | Account::B
                    if row.try_get::<bool, _>("plan_ppu")?
                        || row.try_get::<bool, _>("thread_ppu")? =>
                {
                    Account::Api
                }
                other => other,
            };
            let started: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM events WHERE thread_id = ?
                         AND json_extract(body, '$.kind') IN ('message', 'tool'))",
            )
            .bind(t.id)
            .fetch_one(&mut *tx)
            .await?;
            let resume = t.session_id.is_some() && started;
            // What earlier runs cost, when the agent's own report will count it.
            // The agent's report will count earlier runs too.
            let resumed = resume && t.ai.reports_session_cost();
            let prior = if resumed {
                crate::spend::session_cost_micros(&mut tx, t.id).await?
            } else {
                0
            };
            let reserved =
                match crate::spend::open_run(&mut tx, t.id, t.app_id, account, prior, resumed)
                    .await?
                {
                    Ok(r) => r,
                    Err(hit) => {
                        // The month's pay-per-use money is used up: a hard stop for the owner.
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
                        let note = hit.note();
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
                    }
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
            // Never earlier than any app's stamp, so a clock set back cannot
            // send this app to the front again. A tie goes to the oldest work.
            sqlx::query(
                "UPDATE apps SET last_claim_at = (
                     SELECT MAX(strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), COALESCE(MAX(last_claim_at), ''))
                     FROM apps)
                 WHERE id = ?",
            )
            .bind(t.app_id)
            .execute(&mut *tx)
            .await?;
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
            let mut thread = self.thread(t.id).await?;
            // What other pieces of the same goal found, for this run only:
            // the saved brief stays exactly what the owner approved.
            let mut conn = self.pool.acquire().await?;
            if let Some(n) = crate::coordinator::earlier_pieces_note(&mut conn, t.id).await? {
                thread.prompt.push_str(&n);
            }
            drop(conn);
            let app = self.app(thread.app_id).await?;
            let checkpoint = self.latest_checkpoint(t.id).await?;
            let test_keys = self.test_key_names(app.id).await?;
            // Saved sign-ins never leave the PC; cloud work goes without.
            let sign_ins = match place {
                Place::Pc => self.claim_sign_ins(t.id, app.id).await?,
                Place::Cloud => Vec::new(),
            };
            let context = self.thread_context(t.id).await?;
            let addons = self.claim_addons(app.id).await?;
            return Ok(ClaimOutcome {
                work: Some(Claim {
                    thread,
                    app,
                    resume,
                    account,
                    checkpoint,
                    test_keys,
                    sign_ins,
                    context,
                    addons,
                    budget_usd: account
                        .is_paid()
                        .then(|| crate::spend::claude_budget_usd(prior, reserved)),
                    messages,
                    interrupted,
                    reserved_micros: account.is_paid().then_some(reserved),
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
        if matches!(e, RunEvent::Usage { .. }) {
            return Err(StoreError::Invalid(
                "usage readings are recorded with record_usage".into(),
            ));
        }
        self.thread(thread_id).await?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
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
            tokens,
            ..
        } = e
        {
            crate::spend::record_cost(&mut tx, thread_id, *cost).await?;
            crate::tokens::record_run(
                &mut tx,
                thread_id,
                *input_tokens,
                *output_tokens,
                tokens.as_ref(),
            )
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

fn taken_words(name: &str) -> String {
    format!("Another app is already called {name}. Choose a different name.")
}

/// A GitHub login as GitHub allows them: 1 to 39 letters, digits and single
/// dashes, not starting or ending with a dash. Capitals do not matter to
/// GitHub, so anything comparing logins ignores them.
pub(crate) fn check_reviewer(login: &str) -> Result<()> {
    // The same rule the pull request side uses, so a saved reviewer is
    // always one it will ask.
    if crate::github::is_login(login) {
        Ok(())
    } else {
        Err(StoreError::Invalid(
            "Type the reviewer's GitHub name, like param-k. Only letters, digits and single dashes."
                .into(),
        ))
    }
}

/// Deploy steps as the owner typed them, without blank lines or spaces
/// around each line. Empty clears them; anything else must read cleanly.
fn clean_steps(raw: &str) -> Result<String> {
    // Far more than eight full steps: a guard before reading line by line.
    if raw.len() > 4 * deploy::MAX_STEPS * deploy::MAX_STEP_LEN {
        return Err(StoreError::Invalid(format!(
            "There are too many steps. Keep it to {} short commands.",
            deploy::MAX_STEPS
        )));
    }
    let text = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if !text.is_empty() {
        deploy::parse_steps(&text).map_err(StoreError::Invalid)?;
    }
    Ok(text)
}

/// The reviewer as the owner typed it, without spaces around it or a leading
/// @. Nothing, or only those, means no one.
fn clean_reviewer(raw: Option<&str>) -> Result<Option<String>> {
    let login = raw.unwrap_or("").trim();
    let login = login.strip_prefix('@').unwrap_or(login);
    if login.is_empty() {
        return Ok(None);
    }
    check_reviewer(login)?;
    Ok(Some(login.to_owned()))
}

/// Whether any of the app's work is running, or waits to pick up where its
/// agent left off (the same test as `started` in [`Store::claim`]). That
/// work depends on the runner's folders named after the app.
async fn work_in_progress(tx: &mut Transaction<'_, Sqlite>, app_id: i64) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM threads t WHERE t.app_id = ? AND (t.state = 'running'
             OR (t.state IN ('queued', 'needs_you') AND t.session_id IS NOT NULL
                 AND EXISTS (SELECT 1 FROM events e WHERE e.thread_id = t.id
                     AND json_extract(e.body, '$.kind') IN ('message', 'tool')))))",
    )
    .bind(app_id)
    .fetch_one(&mut **tx)
    .await?)
}

/// Cleans the owner's list of extra websites: each entry becomes a plain site
/// name, repeats and built-in sites are dropped, and anything that is not a
/// site name is refused rather than quietly skipped.
fn clean_sites(input: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for raw in input {
        if raw.trim().is_empty() {
            continue;
        }
        let Some(site) = sites::normalize(raw) else {
            let shown: String = raw.trim().chars().take(80).collect();
            return Err(StoreError::Invalid(format!(
                "\"{shown}\" is not a website name; use a name like stripe.com"
            )));
        };
        if sites::BUILT_IN.contains(&site.as_str()) || out.contains(&site) {
            continue;
        }
        if out.len() == sites::MAX_SITES {
            return Err(StoreError::Invalid(format!(
                "an app can have at most {} extra websites",
                sites::MAX_SITES
            )));
        }
        out.push(site);
    }
    Ok(out)
}

fn sites_json(sites: &[String]) -> Result<String> {
    serde_json::to_string(sites).map_err(|e| StoreError::Invalid(e.to_string()))
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
    // Work that moves again (a retry of dismissed failed work) shows again.
    let done = sqlx::query(
        "UPDATE threads SET state = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
             dismissed_at = NULL
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
