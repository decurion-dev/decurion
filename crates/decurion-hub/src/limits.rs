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

/// Marks the Max account behind a thread's current run as out of usage.
/// Pay-per-use runs are not Max limits and are ignored.
pub(crate) async fn record_limit(
    tx: &mut Transaction<'_, Sqlite>,
    thread_id: i64,
    reset_epoch: Option<i64>,
) -> Result<()> {
    let account: Option<String> =
        sqlx::query_scalar("SELECT account FROM runs WHERE thread_id = ? ORDER BY id DESC LIMIT 1")
            .bind(thread_id)
            .fetch_optional(&mut **tx)
            .await?;
    let Some(profile) = account.filter(|a| a == "A" || a == "B") else {
        return Ok(());
    };
    // A reset time in the past or absurdly far out is not believed.
    let until = match reset_epoch {
        Some(e) => {
            sqlx::query_scalar::<_, String>(
                "SELECT CASE
                 WHEN ? BETWEEN CAST(strftime('%s', 'now') AS INTEGER)
                          AND CAST(strftime('%s', 'now', '+8 days') AS INTEGER)
                 THEN strftime('%Y-%m-%dT%H:%M:%fZ', ?, 'unixepoch')
                 ELSE strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?) END",
            )
            .bind(e)
            .bind(e)
            .bind(DEFAULT_WAIT)
            .fetch_one(&mut **tx)
            .await?
        }
        None => {
            sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?)")
                .bind(DEFAULT_WAIT)
                .fetch_one(&mut **tx)
                .await?
        }
    };
    sqlx::query(
        "INSERT INTO profile_limits (profile, until) VALUES (?, ?)
         ON CONFLICT (profile) DO UPDATE SET until = excluded.until,
             hit_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
    )
    .bind(&profile)
    .bind(&until)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

const ACTIVE: &str = "SELECT l.profile, l.until,
        (SELECT COUNT(*) FROM threads t JOIN apps a ON a.id = t.app_id
         WHERE t.state IN ('queued', 'running') AND a.profile = l.profile
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

    /// Moves every piece of work waiting on `profile` to pay-per-use. Returns
    /// how many moved. It still runs under the monthly cap.
    pub async fn overflow_to_pay_per_use(&self, profile: Profile) -> Result<u64> {
        let done = sqlx::query(
            "UPDATE threads SET pay_per_use = 1
             WHERE state = 'queued' AND pay_per_use = 0
               AND app_id IN (SELECT id FROM apps WHERE profile = ? AND pay_per_use = 0)",
        )
        .bind(profile.as_str())
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected())
    }
}
