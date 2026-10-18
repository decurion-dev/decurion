//! Max accounts running out of usage. When an agent reports the limit, its
//! account is marked out until the reset time Claude gave (or an hour, if it
//! gave none). Work on that account waits in the queue and starts again on
//! its own after that time; if the account is still out, the next attempt
//! reports the limit again and the time moves on.
//!
//! The owner can instead tap "Continue on pay-per-use": until that account's
//! reset time (`pay_per_use_until`), all its Claude work, runs and helper
//! jobs alike, runs on pay-per-use, set aside and capped like any other paid
//! run. After the reset it goes back to the Max account by itself. The tap
//! covers this outage only: a later one waits again until the owner taps
//! again. Work never moves to the other Max account.

use crate::store::{Result, Store, StoreError};
use decurion_proto::{Limit, Profile};
use sqlx::{Row, Sqlite, Transaction};
use std::str::FromStr;

/// How long to wait when Claude did not say when the account resets.
const DEFAULT_WAIT: &str = "+60 minutes";
/// Shortest wait, so a reset time that is already (nearly) past cannot make
/// work retry in a tight loop.
const MIN_WAIT: &str = "+5 minutes";
/// A limit hit again within this long of the first one is the same outage:
/// no new phone alert.
const SAME_OUTAGE: &str = "-6 hours";

/// Marks the Max account behind a thread's last run as out of usage. Only a
/// Max run that just finished counts; pay-per-use runs are not Max limits.
/// Returns `None` when the run was not on a Max account (nothing recorded),
/// otherwise whether this starts a new outage (worth a phone alert).
pub(crate) async fn record_limit(
    tx: &mut Transaction<'_, Sqlite>,
    thread_id: i64,
    reset_epoch: Option<i64>,
) -> Result<Option<bool>> {
    let account: Option<String> = sqlx::query_scalar(
        "SELECT account FROM runs WHERE thread_id = ?
             AND (finished_at IS NULL
                  OR finished_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-10 minutes'))
         ORDER BY id DESC LIMIT 1",
    )
    .bind(thread_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(profile) = account.filter(|a| a == "A" || a == "B") else {
        return Ok(None);
    };
    // Believe Claude's reset time if it is within the next 8 days; never wait
    // less than MIN_WAIT.
    let until: String = sqlx::query_scalar(
        "SELECT MAX(
             CASE WHEN ?1 IS NOT NULL AND ?1 BETWEEN CAST(strftime('%s', 'now') AS INTEGER)
                                   AND CAST(strftime('%s', 'now', '+8 days') AS INTEGER)
                  THEN strftime('%Y-%m-%dT%H:%M:%fZ', ?1, 'unixepoch')
                  ELSE strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?2) END,
             strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?3))",
    )
    .bind(reset_epoch)
    .bind(DEFAULT_WAIT)
    .bind(MIN_WAIT)
    .fetch_one(&mut **tx)
    .await?;
    let new_outage: bool = sqlx::query_scalar(&format!(
        "SELECT NOT EXISTS (SELECT 1 FROM profile_limits WHERE profile = ?
             AND hit_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '{SAME_OUTAGE}'))"
    ))
    .bind(&profile)
    .fetch_one(&mut **tx)
    .await?;
    // An earlier, later-ending report is never shortened; the outage keeps
    // its first hit time so repeat hits do not re-alert. A new outage starts
    // without pay-per-use (the owner decides again); within the outage the
    // owner's pay-per-use follows the reset time if it moves later.
    sqlx::query(
        "INSERT INTO profile_limits (profile, until) VALUES (?1, ?2)
         ON CONFLICT (profile) DO UPDATE SET
             until = CASE WHEN until > strftime('%Y-%m-%dT%H:%M:%fZ', 'now') AND until > excluded.until
                          THEN until ELSE excluded.until END,
             hit_at = CASE WHEN ?3 THEN strftime('%Y-%m-%dT%H:%M:%fZ', 'now') ELSE hit_at END,
             pay_per_use_until = CASE
                 WHEN ?3 THEN NULL
                 WHEN pay_per_use_until > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                     THEN CASE WHEN until > strftime('%Y-%m-%dT%H:%M:%fZ', 'now') AND until > excluded.until
                               THEN until ELSE excluded.until END
                 ELSE pay_per_use_until END",
    )
    .bind(&profile)
    .bind(&until)
    .bind(new_outage)
    .execute(&mut **tx)
    .await?;
    Ok(Some(new_outage))
}

/// A Max account whose limit is active now and the owner chose pay-per-use
/// until its reset. `l` is the `profile_limits` row.
pub(crate) const PAY_PER_USE_NOW: &str = "(l.until > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
     AND l.pay_per_use_until IS NOT NULL
     AND l.pay_per_use_until > strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))";

fn active() -> String {
    format!(
        "SELECT l.profile, l.until,
            CASE WHEN {PAY_PER_USE_NOW} THEN l.pay_per_use_until END AS pay_per_use_until,
            CASE WHEN {PAY_PER_USE_NOW} THEN 0 ELSE
                (SELECT COUNT(*) FROM threads t JOIN apps a ON a.id = t.app_id
                 WHERE t.state = 'queued' AND t.ai = 'claude' AND a.profile = l.profile
                   AND a.pay_per_use = 0 AND t.pay_per_use = 0) END AS waiting
         FROM profile_limits l
         WHERE l.until > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
    )
}

fn limit_from(r: &sqlx::sqlite::SqliteRow) -> Result<Limit> {
    let p: String = r.try_get("profile")?;
    Ok(Limit {
        profile: Profile::from_str(&p).map_err(|e| StoreError::Invalid(e.to_string()))?,
        until: r.try_get("until")?,
        waiting: r.try_get("waiting")?,
        pay_per_use_until: r.try_get("pay_per_use_until")?,
    })
}

/// What a Max account's Claude work does right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MaxPlan {
    /// Not out of usage: work runs on it.
    Ready,
    /// Out of usage: work waits for the reset.
    Out,
    /// Out of usage, and the owner chose pay-per-use until the reset.
    PayPerUse,
}

/// Where `profile`'s Claude work runs now. Read in the claiming
/// transaction, so the answer cannot change before the work starts.
pub(crate) async fn max_plan(tx: &mut Transaction<'_, Sqlite>, profile: &str) -> Result<MaxPlan> {
    let row: Option<(bool,)> = sqlx::query_as(&format!(
        "SELECT {PAY_PER_USE_NOW} FROM profile_limits l
         WHERE l.profile = ? AND l.until > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
    ))
    .bind(profile)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(match row {
        None => MaxPlan::Ready,
        Some((true,)) => MaxPlan::PayPerUse,
        Some((false,)) => MaxPlan::Out,
    })
}

impl Store {
    /// Max accounts that are out of usage right now.
    pub async fn limits(&self) -> Result<Vec<Limit>> {
        let rows = sqlx::query(&format!("{} ORDER BY l.profile", active()))
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(limit_from).collect()
    }

    /// The active limit on the account a thread's app uses, if any.
    pub async fn limit_for_thread(&self, thread_id: i64) -> Result<Option<Limit>> {
        let row = sqlx::query(&format!(
            "{} AND l.profile = (SELECT a.profile FROM threads t JOIN apps a ON a.id = t.app_id
                                       WHERE t.id = ?)",
            active()
        ))
        .bind(thread_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(limit_from).transpose()
    }

    /// The owner's "Continue on pay-per-use" tap: `profile`'s Claude work,
    /// waiting now or added later, runs on pay-per-use until the account's
    /// reset time, then goes back to it. Returns the limit as it now stands;
    /// `None`, and nothing changes, when the account is not out right now (a
    /// stale banner). Every such run is still set aside and checked against
    /// the monthly caps at claim, like any pay-per-use run.
    pub async fn overflow_to_pay_per_use(&self, profile: Profile) -> Result<Option<Limit>> {
        sqlx::query(
            "UPDATE profile_limits SET pay_per_use_until = until
             WHERE profile = ? AND until > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        )
        .bind(profile.as_str())
        .execute(self.pool())
        .await?;
        Ok(self
            .limits()
            .await?
            .into_iter()
            .find(|l| l.profile == profile))
    }
}
