//! Saved website sign-ins: the owner signs in to a site once, in a browser
//! streamed from the PC, and that app's agents start their test browser
//! already signed in.
//!
//! The sign-in itself never reaches the hub: the PC runner seals it in the
//! vault (`decurion_vault::sealed`). This keeps what the board shows (the
//! owner's label, the covered sites, when it runs out) and which sealed copy
//! is current.
//!
//! Walls: a run only gets its own app's sign-ins, and only on the PC. Only
//! the runner that opened a sign-in window may save into it, and only a
//! runner still running work that was given a sign-in may say it stopped
//! working. Inbox cards and notifications are made from the owner's label
//! and the app's name alone, never from anything a site or an agent said.

use crate::store::{Result, Store, StoreError};
use crate::workers::RunnerId;
use crate::{forbidden, no_vault, runner_only, ApiError, AppState, Caller};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use decurion_proto::{Place, SignInRef, SignInSaved, SignInSession};
use serde::{Deserialize, Serialize};
use sqlx::Row;

/// A sign-in counts as running out this long before its cookies end.
pub const EXPIRING_HOURS: i64 = 24;
/// A sign-in window nobody saved from is given up after this long.
const STALE_MINUTES: i64 = 30;
/// The most sign-ins one app may have: the runner hands at most this many
/// to one browser (`MAX_STATES` in the runner).
const MAX_SIGN_INS: i64 = 16;
/// How often the hub records sign-ins that ran out by date, and tells the
/// owner. Lists show the status by date at once either way.
const WATCH_EVERY: std::time::Duration = std::time::Duration::from_secs(5 * 60);

const MAX_LABEL: usize = 60;
const MAX_URL: usize = 2000;
const MAX_DOMAINS: usize = 50;

const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')";

/// The routes, for `lib.rs` to add to the API.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/sign-ins", get(list_all))
        .route("/apps/{id}/sign-ins", get(list).post(add))
        .route("/sign-ins/{id}/sign-in", post(start))
        .route("/sign-ins/{id}/remove", post(remove))
        .route("/browser/{sid}/save", post(save_window))
        .route("/runner/browser/claim", post(runner_claim))
        .route("/runner/sign-ins/{id}/saved", post(runner_saved))
        .route("/runner/sign-ins/{id}/expired", post(runner_expired))
}

/// Whether a saved sign-in still works, as the board shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignInStatus {
    /// Added, but never signed in to.
    None,
    Ok,
    /// Its cookies run out within a day.
    Expiring,
    /// Its cookies ran out, or a run found the site signed out.
    Expired,
}

impl SignInStatus {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "none" => Ok(Self::None),
            "ok" => Ok(Self::Ok),
            "expiring" => Ok(Self::Expiring),
            "expired" => Ok(Self::Expired),
            other => Err(StoreError::Invalid(format!(
                "unknown sign-in status {other}"
            ))),
        }
    }
}

/// A saved sign-in as the board and inbox see it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SiteSignIn {
    pub id: i64,
    pub app_id: i64,
    pub app_name: String,
    pub label: String,
    pub start_url: String,
    pub check_url: Option<String>,
    pub login_url: Option<String>,
    /// The sites it covers.
    pub domains: Vec<String>,
    pub status: SignInStatus,
    pub version: i64,
    pub captured_at: Option<String>,
    pub expires_at: Option<String>,
    pub added_at: String,
    /// The inbox card's title, when the owner needs to sign in again. Made
    /// from the owner's label only.
    pub card: Option<String>,
}

/// The board's "add a website sign-in" form.
#[derive(Debug, Deserialize)]
pub struct NewSignIn {
    pub label: String,
    pub start_url: String,
    #[serde(default)]
    pub check_url: Option<String>,
}

/// The inbox card and notification title for a sign-in that needs the owner.
pub fn card_title(label: &str) -> String {
    format!("Sign in to {label} again")
}

/// A sign-in's status now: what is stored, moved on by its cookie date. The
/// stored one catches up in [`Store::settle_sign_ins`]; reading never writes.
fn status_now() -> String {
    format!(
        "CASE
             WHEN s.status IN ('ok', 'expiring') AND s.expires_at <= {NOW} THEN 'expired'
             WHEN s.status = 'ok'
                 AND s.expires_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+{EXPIRING_HOURS} hours')
                 THEN 'expiring'
             ELSE s.status
         END"
    )
}

fn cols() -> String {
    format!(
        "s.id, s.app_id, a.name AS app_name, s.label, s.start_url, s.check_url, s.login_url,
         s.domains, {} AS status, s.version, s.captured_at, s.expires_at, s.added_at",
        status_now()
    )
}

fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<SiteSignIn> {
    let status = SignInStatus::parse(&row.try_get::<String, _>("status")?)?;
    let label: String = row.try_get("label")?;
    let domains: String = row.try_get("domains")?;
    Ok(SiteSignIn {
        id: row.try_get("id")?,
        app_id: row.try_get("app_id")?,
        app_name: row.try_get("app_name")?,
        card: matches!(status, SignInStatus::Expiring | SignInStatus::Expired)
            .then(|| card_title(&label)),
        label,
        start_url: row.try_get("start_url")?,
        check_url: row.try_get("check_url")?,
        login_url: row.try_get("login_url")?,
        domains: serde_json::from_str(&domains).unwrap_or_default(),
        status,
        version: row.try_get("version")?,
        captured_at: row.try_get("captured_at")?,
        expires_at: row.try_get("expires_at")?,
        added_at: row.try_get("added_at")?,
    })
}

/// The owner's name for a sign-in: one line, 1 to 60 characters.
fn check_label(label: &str) -> Result<String> {
    let label = label.trim();
    let n = label.chars().count();
    if !(1..=MAX_LABEL).contains(&n) || label.chars().any(char::is_control) {
        return Err(StoreError::Invalid(format!(
            "give the sign-in a name of 1 to {MAX_LABEL} characters, on one line"
        )));
    }
    Ok(label.to_owned())
}

/// An address the owner gave: https, with no name or password in it. Not
/// `localhost`: the sign-in window is a box of its own, where that would be
/// the box itself, not the owner's PC.
fn check_owner_url(what: &str, raw: &str) -> Result<String> {
    let bad = || {
        StoreError::Invalid(format!(
            "the {what} must start with https://. Sites running on your PC can't be signed in to yet"
        ))
    };
    let raw = raw.trim();
    if raw.len() > MAX_URL {
        return Err(StoreError::Invalid(format!("the {what} is too long")));
    }
    let u = url::Url::parse(raw).map_err(|_| bad())?;
    let host = u.host_str().unwrap_or("");
    let ok = match u.scheme() {
        "https" => !host.is_empty() && host != "localhost",
        _ => false,
    };
    if !ok {
        return Err(bad());
    }
    // A password in an address would be saved here in plain text.
    if !u.username().is_empty() || u.password().is_some() {
        return Err(StoreError::Invalid(format!(
            "leave the name and password out of the {what}; you type them in the sign-in window"
        )));
    }
    Ok(u.to_string())
}

/// Where the site sent the browser to sign in, as the runner reported it.
/// Kept only if it is an ordinary web address.
fn clean_login_url(raw: Option<&str>) -> Option<String> {
    let u = url::Url::parse(raw?.trim()).ok()?;
    let ok = matches!(u.scheme(), "http" | "https")
        && u.host_str().is_some()
        && u.username().is_empty()
        && u.password().is_none()
        && u.as_str().len() <= MAX_URL;
    ok.then(|| u.to_string())
}

/// The covered sites, as the runner reported them: host names only, so the
/// board never shows anything a site made up.
fn clean_domains(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for d in raw {
        let d = d.trim().trim_start_matches('.').to_ascii_lowercase();
        let ok = (1..=253).contains(&d.len())
            && d.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
            && !d.starts_with('-')
            && !d.contains("..");
        if ok && !out.contains(&d) {
            out.push(d);
        }
        if out.len() == MAX_DOMAINS {
            break;
        }
    }
    out.sort();
    out
}

/// A cookie end date in Unix seconds, or `None` for one outside the years
/// 1970 to 9999, which no real cookie has.
fn when(expires_at: Option<i64>) -> Option<i64> {
    expires_at.filter(|s| (0..253_402_300_800).contains(s))
}

impl Store {
    /// Moves sign-ins on by date: to expiring a day before their cookies end,
    /// and to expired once they have. Returns the ones that just expired, so
    /// the owner can be told once.
    pub async fn settle_sign_ins(&self) -> Result<Vec<SiteSignIn>> {
        let now = status_now();
        let ids: Vec<(i64, String)> = sqlx::query_as(&format!(
            "UPDATE site_sign_ins AS s SET status = {now}
             WHERE s.expires_at IS NOT NULL AND s.status <> {now}
             RETURNING id, status"
        ))
        .fetch_all(self.pool())
        .await?;
        let mut out = Vec::new();
        for (id, status) in ids {
            if status == "expired" {
                out.push(self.site_sign_in(id).await?);
            }
        }
        Ok(out)
    }

    pub async fn site_sign_in(&self, id: i64) -> Result<SiteSignIn> {
        let row = sqlx::query(&format!(
            "SELECT {} FROM site_sign_ins s JOIN apps a ON a.id = s.app_id WHERE s.id = ?",
            cols()
        ))
        .bind(id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(StoreError::NotFound)?;
        from_row(&row)
    }

    /// One app's sign-ins, by label.
    pub async fn site_sign_ins(&self, app_id: i64) -> Result<Vec<SiteSignIn>> {
        self.app(app_id).await?;
        let rows = sqlx::query(&format!(
            "SELECT {} FROM site_sign_ins s JOIN apps a ON a.id = s.app_id
             WHERE s.app_id = ? ORDER BY s.label",
            cols()
        ))
        .bind(app_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(from_row).collect()
    }

    /// Every app's sign-ins, for the inbox.
    pub async fn all_site_sign_ins(&self) -> Result<Vec<SiteSignIn>> {
        let rows = sqlx::query(&format!(
            "SELECT {} FROM site_sign_ins s JOIN apps a ON a.id = s.app_id
             ORDER BY a.name, s.label",
            cols()
        ))
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(from_row).collect()
    }

    pub async fn add_site_sign_in(&self, app_id: i64, n: &NewSignIn) -> Result<SiteSignIn> {
        let label = check_label(&n.label)?;
        let start = check_owner_url("address", &n.start_url)?;
        let check = match n.check_url.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(c) => Some(check_owner_url("page to check", c)?),
        };
        self.app(app_id).await?;
        let have: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM site_sign_ins WHERE app_id = ?")
            .bind(app_id)
            .fetch_one(self.pool())
            .await?;
        if have >= MAX_SIGN_INS {
            return Err(StoreError::Refused(format!(
                "an app can have at most {MAX_SIGN_INS} website sign-ins; remove one you no longer need first"
            )));
        }
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO site_sign_ins (app_id, label, start_url, check_url) VALUES (?, ?, ?, ?)
             RETURNING id",
        )
        .bind(app_id)
        .bind(&label)
        .bind(&start)
        .bind(&check)
        .fetch_one(self.pool())
        .await
        .map_err(|e| match e.as_database_error() {
            Some(d) if d.is_unique_violation() => {
                StoreError::Duplicate(format!("this app already has a sign-in called {label}"))
            }
            _ => StoreError::Db(e),
        })?;
        self.site_sign_in(id).await
    }

    /// Opens a sign-in window for the owner: a session the PC runner picks
    /// up. A window already waiting or open is reused, so a second tap does
    /// not start a second browser. Returns the session and whether it is new.
    pub async fn start_sign_in(&self, id: i64) -> Result<(i64, bool)> {
        self.site_sign_in(id).await?;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        close_stale(&mut tx).await?;
        let open: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM browser_sessions
             WHERE kind = 'sign_in' AND sign_in_id = ? AND state IN ('pending', 'live')
             ORDER BY id DESC LIMIT 1",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(sid) = open {
            tx.commit().await?;
            return Ok((sid, false));
        }
        // The owner drives a sign-in window from the start; no agent is in it.
        let sid: i64 = sqlx::query_scalar(&format!(
            "INSERT INTO browser_sessions (kind, sign_in_id, controller, state, created_at)
             VALUES ('sign_in', ?, 'owner', 'pending', {NOW}) RETURNING id"
        ))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((sid, true))
    }

    /// Forgets a sign-in and its windows. The vault copy is removed first, by
    /// the caller.
    pub async fn remove_site_sign_in(&self, id: i64) -> Result<()> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("DELETE FROM browser_sessions WHERE sign_in_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM site_sign_ins WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The PC runner takes the oldest waiting sign-in window.
    pub async fn claim_sign_in_window(&self, runner: &RunnerId) -> Result<Option<SignInSession>> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        close_stale(&mut tx).await?;
        let row = sqlx::query(
            "SELECT b.id AS session_id, s.id AS sign_in_id, s.app_id, s.start_url
             FROM browser_sessions b JOIN site_sign_ins s ON s.id = b.sign_in_id
             WHERE b.kind = 'sign_in' AND b.state = 'pending'
             ORDER BY b.id LIMIT 1",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let session_id: i64 = row.try_get("session_id")?;
        let done = sqlx::query(
            "UPDATE browser_sessions SET state = 'live', runner = ? WHERE id = ? AND state = 'pending'",
        )
        .bind(&runner.name)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
        if done.rows_affected() != 1 {
            return Err(StoreError::Conflict);
        }
        tx.commit().await?;
        Ok(Some(SignInSession {
            session_id,
            sign_in_id: row.try_get("sign_in_id")?,
            app_id: row.try_get("app_id")?,
            start_url: row.try_get("start_url")?,
            // Session ids only grow, so each window seals to a file of its own.
            version: session_id,
        }))
    }

    /// Records a sign-in the runner sealed in the vault, from the window it
    /// opened. Anything else is refused.
    pub async fn sign_in_saved(&self, id: i64, runner: &RunnerId, s: &SignInSaved) -> Result<()> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let current: Option<i64> =
            sqlx::query_scalar("SELECT version FROM site_sign_ins WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let current = current.ok_or(StoreError::NotFound)?;
        let window = sqlx::query(
            "SELECT kind, sign_in_id, runner, state FROM browser_sessions WHERE id = ?",
        )
        .bind(s.session_id)
        .fetch_optional(&mut *tx)
        .await?;
        let ours = match &window {
            Some(w) => {
                w.try_get::<String, _>("kind")? == "sign_in"
                    && w.try_get::<Option<i64>, _>("sign_in_id")? == Some(id)
                    && w.try_get::<Option<String>, _>("runner")?.as_deref()
                        == Some(runner.name.as_str())
                    && w.try_get::<String, _>("state")? == "live"
            }
            None => false,
        };
        if !ours {
            return Err(StoreError::Refused(
                "that sign-in window is not open on this runner".into(),
            ));
        }
        if s.version != s.session_id || s.version <= current {
            return Err(StoreError::Refused(
                "that is not the version this window saves as".into(),
            ));
        }
        let domains =
            serde_json::to_string(&clean_domains(&s.domains)).unwrap_or_else(|_| "[]".into());
        sqlx::query(&format!(
            "UPDATE site_sign_ins SET status = 'ok', version = ?, login_url = ?, domains = ?,
                 expires_at = strftime('%Y-%m-%dT%H:%M:%fZ', ?, 'unixepoch'), captured_at = {NOW}
             WHERE id = ?"
        ))
        .bind(s.version)
        .bind(clean_login_url(s.login_url.as_deref()))
        .bind(domains)
        .bind(when(s.expires_at))
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(&format!(
            "UPDATE browser_sessions SET state = 'saved', ended_at = {NOW} WHERE id = ?"
        ))
        .bind(s.session_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// A run found a sign-in no longer works. Only a runner still running
    /// work of that sign-in's app that was given it may say so, and only
    /// about the copy it was given: a newer one the owner saved since is
    /// left alone. `run` names the piece of work and the copy it was given;
    /// with several pieces running on one runner, that is the only way to
    /// know which copy the report is about. Without it (an older runner),
    /// the newest copy given to any of this runner's running work counts.
    /// Returns the sign-in if this is news.
    pub async fn sign_in_expired(
        &self,
        id: i64,
        runner: &RunnerId,
        run: Option<(i64, i64)>,
    ) -> Result<Option<SiteSignIn>> {
        self.site_sign_in(id).await?;
        let (thread, version) = run.unzip();
        let given: Option<i64> = sqlx::query_scalar(
            "SELECT r.version FROM sign_in_runs r
             JOIN threads t ON t.id = r.thread_id
             JOIN site_sign_ins s ON s.id = r.sign_in_id
             WHERE r.sign_in_id = ? AND t.state = 'running' AND t.runner = ?
               AND t.app_id = s.app_id
               AND (?3 IS NULL OR (r.thread_id = ?3 AND r.version = ?4))
             ORDER BY r.version DESC LIMIT 1",
        )
        .bind(id)
        .bind(runner.actor())
        .bind(thread)
        .bind(version)
        .fetch_optional(self.pool())
        .await?;
        let Some(version) = given else {
            return Err(StoreError::Refused(
                "no work running on this runner was given that sign-in".into(),
            ));
        };
        let done = sqlx::query(
            "UPDATE site_sign_ins SET status = 'expired'
             WHERE id = ? AND version = ? AND status IN ('ok', 'expiring')",
        )
        .bind(id)
        .bind(version)
        .execute(self.pool())
        .await?;
        if done.rows_affected() == 1 {
            Ok(Some(self.site_sign_in(id).await?))
        } else {
            Ok(None)
        }
    }

    /// The app's working sign-ins, for a claim, recorded as given to this
    /// run. Ones out by date are left out even before they are marked.
    pub(crate) async fn claim_sign_ins(
        &self,
        thread_id: i64,
        app_id: i64,
    ) -> Result<Vec<SignInRef>> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let rows = sqlx::query(&format!(
            "SELECT id, label, check_url, login_url, version FROM site_sign_ins
             WHERE app_id = ? AND status IN ('ok', 'expiring') AND version > 0
               AND (expires_at IS NULL OR expires_at > {NOW})
             ORDER BY label"
        ))
        .bind(app_id)
        .fetch_all(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM sign_in_runs WHERE thread_id = ?")
            .bind(thread_id)
            .execute(&mut *tx)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let r = SignInRef {
                id: row.try_get("id")?,
                label: row.try_get("label")?,
                check_url: row.try_get("check_url")?,
                login_url: row.try_get("login_url")?,
                version: row.try_get("version")?,
            };
            sqlx::query(
                "INSERT INTO sign_in_runs (thread_id, sign_in_id, version) VALUES (?, ?, ?)",
            )
            .bind(thread_id)
            .bind(r.id)
            .bind(r.version)
            .execute(&mut *tx)
            .await?;
            out.push(r);
        }
        tx.commit().await?;
        Ok(out)
    }
}

/// Sign-in windows nobody saved from in time are closed.
async fn close_stale(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<()> {
    sqlx::query(&format!(
        "UPDATE browser_sessions SET state = 'closed', ended_at = {NOW}
         WHERE kind = 'sign_in' AND state IN ('pending', 'live')
           AND created_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-{STALE_MINUTES} minutes')"
    ))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Tells the owner's phones, once per sign-in that stopped working.
fn tell_expired(st: &AppState, expired: Vec<SiteSignIn>) {
    let Some(p) = st.push.clone() else { return };
    for s in expired {
        let store = st.store.clone();
        let p = p.clone();
        let title = card_title(&s.label);
        let body = format!(
            "{}: its saved sign-in stopped working. Tap to sign in again.",
            s.app_name
        );
        tokio::spawn(async move { p.notify(&store, &title, &body).await });
    }
}

/// Marks sign-ins out by date and tells the owner about each newly expired.
async fn settle(st: &AppState) {
    match st.store.settle_sign_ins().await {
        Ok(expired) => tell_expired(st, expired),
        Err(e) => tracing::error!(error = %e, "could not update saved sign-ins"),
    }
}

/// Keeps sign-in statuses current by date even while nobody looks, so the
/// notification comes when one runs out. Started by the hub's `main`.
pub fn watch_dates(st: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(WATCH_EVERY);
        loop {
            tick.tick().await;
            settle(&st).await;
        }
    });
}

fn reply<T: Serialize>(r: Result<T>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn list_all(State(st): State<AppState>, Extension(c): Extension<Caller>) -> Response {
    if c != Caller::Owner {
        return forbidden("seeing website sign-ins");
    }
    reply(st.store.all_site_sign_ins().await)
}

async fn list(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(app): Path<i64>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("seeing website sign-ins");
    }
    reply(st.store.site_sign_ins(app).await)
}

async fn add(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(app): Path<i64>,
    Json(n): Json<NewSignIn>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("adding website sign-ins");
    }
    if st.vault.is_none() {
        return no_vault();
    }
    match st.store.add_site_sign_in(app, &n).await {
        Ok(s) => (StatusCode::CREATED, Json(s)).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// The owner's "Sign in" tap: a window for the PC runner to open. The board
/// then shows it with the browser viewer.
async fn start(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("signing in to a website");
    }
    if st.vault.is_none() {
        return no_vault();
    }
    match st.store.start_sign_in(id).await {
        Ok((session_id, fresh)) => {
            crate::created_or_resent(serde_json::json!({ "session_id": session_id }), fresh)
        }
        Err(e) => ApiError(e).into_response(),
    }
}

/// The owner's "I'm signed in, save" in a sign-in window: the runner holding
/// the window captures what the owner signed in to and reports it saved.
async fn save_window(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(sid): Path<i64>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("saving a website sign-in");
    }
    match st.store.browser_session(sid).await {
        Ok(s) if s.kind == "sign_in" && s.is_open() => {}
        Ok(_) => return (StatusCode::CONFLICT, "that sign-in window is not open").into_response(),
        Err(e) => return ApiError(e).into_response(),
    }
    if st
        .browser
        .send_to_runner(sid, &decurion_proto::browser::RelayDown::Capture)
    {
        StatusCode::ACCEPTED.into_response()
    } else {
        (
            StatusCode::CONFLICT,
            "the sign-in window is not connected to your PC yet; try again in a moment",
        )
            .into_response()
    }
}

/// Forgets a sign-in: the vault copy first, then the row, so a failed
/// removal still shows on the board and can be tried again.
async fn remove(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("removing website sign-ins");
    }
    let Some(vault) = st.vault.clone() else {
        return no_vault();
    };
    let s = match st.store.site_sign_in(id).await {
        Ok(s) => s,
        Err(e) => return ApiError(e).into_response(),
    };
    let app = s.app_id;
    match tokio::task::spawn_blocking(move || vault.delete_sign_in(app, id)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::error!(error = %e, app, sign_in = id, "key store error");
            return crate::key_store_failed();
        }
        Err(e) => {
            tracing::error!(error = %e, app, sign_in = id, "key store task failed");
            return crate::key_store_failed();
        }
    }
    match st.store.remove_site_sign_in(id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// Sign-ins live in the PC's vault, so only the PC runner handles them. A
/// cloud worker's key never reaches these routes anyway (`cloud_reach`).
// Responses are what axum handlers return; boxing them buys nothing here.
#[allow(clippy::result_large_err)]
fn pc_runner(runner: Option<Extension<RunnerId>>) -> std::result::Result<RunnerId, Response> {
    let r = runner_only(runner, None)?;
    if r.place != Place::Pc {
        return Err(forbidden("website sign-ins stay on the PC; this"));
    }
    Ok(r)
}

async fn runner_claim(State(st): State<AppState>, runner: Option<Extension<RunnerId>>) -> Response {
    let r = match pc_runner(runner) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match st.store.claim_sign_in_window(&r).await {
        Ok(Some(s)) => Json(s).into_response(),
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

async fn runner_saved(
    State(st): State<AppState>,
    runner: Option<Extension<RunnerId>>,
    Path(id): Path<i64>,
    Json(s): Json<SignInSaved>,
) -> Response {
    let r = match pc_runner(runner) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if let Err(e) = st.store.sign_in_saved(id, &r, &s).await {
        return ApiError(e).into_response();
    }
    // Records one saved already near or past its end, and tells the owner.
    settle(&st).await;
    reply(st.store.site_sign_in(id).await)
}

/// Which piece of work found the sign-in no longer works, and the copy it
/// was given. Older runners send neither.
#[derive(Deserialize, Default)]
struct ExpiredReport {
    #[serde(default)]
    thread: Option<i64>,
    #[serde(default)]
    version: Option<i64>,
}

async fn runner_expired(
    State(st): State<AppState>,
    runner: Option<Extension<RunnerId>>,
    Path(id): Path<i64>,
    Json(report): Json<Option<ExpiredReport>>,
) -> Response {
    let r = match pc_runner(runner) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let given = match report.unwrap_or_default() {
        ExpiredReport {
            thread: Some(t),
            version: Some(v),
        } => Some((t, v)),
        ExpiredReport {
            thread: None,
            version: None,
        } => None,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "send the thread and the version together",
            )
                .into_response()
        }
    };
    match st.store.sign_in_expired(id, &r, given).await {
        Ok(news) => {
            tell_expired(&st, news.into_iter().collect());
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => ApiError(e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_addresses_are_https_and_not_this_pc() {
        assert!(check_owner_url("address", " https://staging.example.com/login ").is_ok());
        for bad in [
            // The sign-in window's own box, not the owner's PC.
            "http://localhost:3000/admin",
            "https://localhost/admin",
            "http://staging.example.com",
            "http://127.0.0.1:3000",
            "http://localhost.evil.com",
            "ftp://example.com",
            "javascript:alert(1)",
            "https://",
            "staging.example.com",
            "https://me:hunter2@example.com",
            "https://me@example.com",
        ] {
            assert!(check_owner_url("address", bad).is_err(), "{bad}");
        }
        assert!(
            check_owner_url("address", &format!("https://e.com/{}", "a".repeat(MAX_URL))).is_err()
        );
    }

    #[test]
    fn labels_are_one_short_line() {
        assert_eq!(
            check_label("  Staging admin ").ok().as_deref(),
            Some("Staging admin")
        );
        assert!(check_label("").is_err());
        assert!(check_label("two\nlines").is_err());
        assert!(check_label(&"x".repeat(MAX_LABEL + 1)).is_err());
    }

    #[test]
    fn what_the_runner_reports_is_cleaned() {
        assert_eq!(
            clean_domains(&[
                ".Example.com".into(),
                "example.com".into(),
                "api.example.com".into(),
                "Sign in to evil".into(),
                "<b>x</b>".into(),
                "a..b".into(),
            ]),
            vec!["api.example.com".to_owned(), "example.com".to_owned()]
        );
        assert_eq!(
            clean_login_url(Some("https://example.com/login?next=/")).as_deref(),
            Some("https://example.com/login?next=/")
        );
        assert_eq!(clean_login_url(Some("javascript:alert(1)")), None);
        assert_eq!(clean_login_url(Some("https://u:p@example.com/")), None);
        assert_eq!(when(Some(-1)), None);
        assert_eq!(when(Some(1_800_000_000)), Some(1_800_000_000));
    }
}
