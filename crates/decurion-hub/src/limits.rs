//! Max accounts running out of usage. When an agent reports the limit, its
//! account is marked out until the reset time Claude gave (or an hour, if it
//! gave none). Work on that account waits in the queue and starts again on
//! its own after that time; if the account is still out, the next attempt
//! reports the limit again and the time moves on. The owner can instead move
//! the waiting work to pay-per-use with one tap.

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
    // its first hit time so repeat hits do not re-alert.
    sqlx::query(
        "INSERT INTO profile_limits (profile, until) VALUES (?1, ?2)
         ON CONFLICT (profile) DO UPDATE SET
             until = CASE WHEN until > strftime('%Y-%m-%dT%H:%M:%fZ', 'now') AND until > excluded.until
                          THEN until ELSE excluded.until END,
             hit_at = CASE WHEN ?3 THEN strftime('%Y-%m-%dT%H:%M:%fZ', 'now') ELSE hit_at END",
    )
    .bind(&profile)
    .bind(&until)
    .bind(new_outage)
    .execute(&mut **tx)
    .await?;
    Ok(Some(new_outage))
}

const ACTIVE: &str = "SELECT l.profile, l.until,
        (SELECT COUNT(*) FROM threads t JOIN apps a ON a.id = t.app_id
         WHERE t.state = 'queued' AND a.profile = l.profile
           AND a.pay_per_use = 0 AND t.pay_per_use = 0) AS waiting
     FROM profile_limits l
     WHERE l.until > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')";

fn limit_from(r: &sqlx::sqlite::SqliteRow) -> Result<Limit> {
    let p: String = r.try_get("profile")?;
    Ok(Limit {
        profile: Profile::from_str(&p).map_err(|e| StoreError::Invalid(e.to_string()))?,
        until: r.try_get("until")?,
        waiting: r.try_get("waiting")?,
    })
}

impl Store {
    /// Max accounts that are out of usage right now.
    pub async fn limits(&self) -> Result<Vec<Limit>> {
        let rows = sqlx::query(&format!("{ACTIVE} ORDER BY l.profile"))
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(limit_from).collect()
    }

    /// The active limit on the account a thread's app uses, if any.
    pub async fn limit_for_thread(&self, thread_id: i64) -> Result<Option<Limit>> {
        let row = sqlx::query(&format!(
            "{ACTIVE} AND l.profile = (SELECT a.profile FROM threads t JOIN apps a ON a.id = t.app_id
                                       WHERE t.id = ?)"
        ))
        .bind(thread_id)
        .fetch_optional(self.pool())
        .await?;
        row.as_ref().map(limit_from).transpose()
    }

    /// Moves every piece of work waiting on `profile` to pay-per-use, for
    /// its next run only. Returns how many moved; nothing moves if the account
    /// is not actually out (a stale banner). It still runs under the monthly
    /// cap.
    pub async fn overflow_to_pay_per_use(&self, profile: Profile) -> Result<u64> {
        let done = sqlx::query(
            "UPDATE threads SET pay_per_use = 1
             WHERE state = 'queued' AND pay_per_use = 0
               AND app_id IN (SELECT id FROM apps WHERE profile = ?1 AND pay_per_use = 0)
               AND EXISTS (SELECT 1 FROM profile_limits WHERE profile = ?1
                           AND until > strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        )
        .bind(profile.as_str())
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected())
    }
}
