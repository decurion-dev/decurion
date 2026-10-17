//! Max plan meters. While it runs, Claude Code reports how much of the plan's
//! five-hour and weekly allowances are used. The hub keeps the latest reading
//! per plan and window and shows it on the board. Readings are not activity,
//! so they are not saved as events; they are saved here before the board can
//! show them.
//!
//! This only records. Deciding what to do when a plan runs out is not done
//! here.

use crate::store::{Result, Store, StoreError};
use decurion_proto::{Account, AccountMeter, RunEvent};
use sqlx::{Row, Sqlite, Transaction};

/// Claude's window names are short snake_case words; anything else is not a
/// reading worth keeping. A reading without one is kept as `plan`.
fn window_name(w: Option<&str>) -> Option<&str> {
    match w {
        None => Some("plan"),
        Some(w)
            if (1..=40).contains(&w.len())
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') =>
        {
            Some(w)
        }
        Some(_) => None,
    }
}

const STATUSES: [&str; 3] = ["allowed", "allowed_warning", "rejected"];

/// Reset times outside 2020-2100 are not real.
const RESET_RANGE: std::ops::Range<i64> = 1_577_836_800..4_102_444_800;

/// The weekly meter shows the fullest of the plan's weekly allowances (the
/// plan-wide one and any per-model ones), but not extra paid usage.
fn is_weekly(window: &str) -> bool {
    window.starts_with("seven_day") && !window.contains("overage")
}

impl Store {
    /// Records a usage reading against the Max plan the thread's current run
    /// is on. Readings for a thread with no open run, or whose run is on a
    /// paid account, are dropped. Returns whether it was kept.
    pub async fn record_usage(&self, thread_id: i64, e: &RunEvent) -> Result<bool> {
        let RunEvent::Usage {
            window,
            used_pct,
            resets_at,
            status,
        } = e
        else {
            return Err(StoreError::Invalid("not a usage reading".into()));
        };
        self.thread(thread_id).await?;
        let Some(window) = window_name(window.as_deref()) else {
            return Ok(false);
        };
        let pct = used_pct
            .filter(|p| p.is_finite())
            .map(|p| p.clamp(0.0, 100.0));
        let resets = resets_at.filter(|t| RESET_RANGE.contains(t));
        let status = status.as_deref().filter(|s| STATUSES.contains(s));
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let account: Option<String> = sqlx::query_scalar(
            "SELECT account FROM runs WHERE thread_id = ? AND finished_at IS NULL
             ORDER BY id DESC LIMIT 1",
        )
        .bind(thread_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(account) = account
            .and_then(|a| a.parse::<Account>().ok())
            .filter(|a| !a.is_paid())
        else {
            return Ok(false);
        };
        let seq: Option<i64> = match status {
            Some(_) => Some(
                sqlx::query_scalar("SELECT COALESCE(MAX(status_seq), 0) + 1 FROM account_usage")
                    .fetch_one(&mut *tx)
                    .await?,
            ),
            None => None,
        };
        // A reading without a figure keeps the last one, unless it says the
        // window has moved on, when the old figure no longer applies.
        sqlx::query(
            "INSERT INTO account_usage (account, window_name, used_pct, resets_at, status, status_seq)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (account, window_name) DO UPDATE SET
                 used_pct = CASE
                     WHEN excluded.used_pct IS NOT NULL THEN excluded.used_pct
                     WHEN excluded.resets_at IS NOT NULL AND excluded.resets_at IS NOT resets_at
                         THEN NULL
                     ELSE used_pct END,
                 resets_at = COALESCE(excluded.resets_at, resets_at),
                 status = COALESCE(excluded.status, status),
                 status_seq = COALESCE(excluded.status_seq, status_seq),
                 seen_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        )
        .bind(account.as_str())
        .bind(window)
        .bind(pct)
        .bind(resets)
        .bind(status)
        .bind(seq)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }
}

/// Fills a Max plan's meter from its latest readings. A window whose reset
/// time has passed has started again from zero.
pub(crate) async fn fill_meter(
    tx: &mut Transaction<'_, Sqlite>,
    m: &mut AccountMeter,
) -> Result<()> {
    let rows = sqlx::query(
        "SELECT window_name, used_pct, status, status_seq, seen_at,
                strftime('%Y-%m-%dT%H:%M:%SZ', resets_at, 'unixepoch') AS resets_iso,
                COALESCE(resets_at <= CAST(strftime('%s', 'now') AS INTEGER), 0) AS renewed
         FROM account_usage WHERE account = ?",
    )
    .bind(m.account.as_str())
    .fetch_all(&mut **tx)
    .await?;
    // (share used, when it renews) of the fullest window shown on the meter.
    let mut fullest: Option<(f64, Option<String>)> = None;
    // (order, rejected, when it renews) of the newest status.
    let mut newest: Option<(i64, bool, Option<String>)> = None;
    for r in &rows {
        let window: String = r.try_get("window_name")?;
        let renewed: bool = r.try_get("renewed")?;
        let resets: Option<String> = r.try_get("resets_iso")?;
        let seen: String = r.try_get("seen_at")?;
        if m.seen_at.as_ref().is_none_or(|s| *s < seen) {
            m.seen_at = Some(seen);
        }
        let pct = r
            .try_get::<Option<f64>, _>("used_pct")?
            .map(|p| if renewed { 0.0 } else { p });
        if let (Some(seq), Some(status)) = (
            r.try_get::<Option<i64>, _>("status_seq")?,
            r.try_get::<Option<String>, _>("status")?,
        ) {
            if newest.as_ref().is_none_or(|(n, ..)| *n < seq) {
                newest = Some((seq, status == "rejected" && !renewed, resets.clone()));
            }
        }
        let Some(pct) = pct else { continue };
        // A renewed window's reset time has passed and is no use to show.
        let resets = resets.filter(|_| !renewed);
        if window == "five_hour" {
            m.five_hour_pct = Some(pct);
            m.five_hour_resets_at.clone_from(&resets);
        } else if is_weekly(&window) {
            if m.week_pct.is_none_or(|w| w < pct) {
                m.week_pct = Some(pct);
                m.week_resets_at.clone_from(&resets);
            }
        } else {
            continue;
        }
        if !renewed && fullest.as_ref().is_none_or(|(p, _)| *p < pct) {
            fullest = Some((pct, resets));
        }
    }
    m.limited = newest.as_ref().is_some_and(|(_, rejected, _)| *rejected);
    m.resets_at = match newest {
        Some((_, true, resets)) => resets,
        _ => fullest.and_then(|(_, resets)| resets),
    };
    Ok(())
}
