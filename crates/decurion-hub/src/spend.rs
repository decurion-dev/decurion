//! Money. Every run gets a row in `runs`. A pay-per-use run sets money aside
//! from the monthly cap before it starts, and Claude Code is given most of it
//! as its own limit (`--max-budget-usd`).
//!
//! Claude Code checks that limit between model calls, so a run can go over by
//! at most one call; the 10% held back covers most of that, and the real cost
//! is what gets recorded. The cap is therefore Decurion's control, not an
//! absolute one. The absolute backstop is the spend limit on the Anthropic
//! Console workspace the API key belongs to, which should equal this cap.
//!
//! When a run's real cost is unknown (the runner died, the container failed,
//! or the cost report could not be trusted), the hub counts the whole
//! set-aside amount if the container started at all. Erring high keeps the
//! cap a real limit. Months are calendar months in UTC; a run counts in the
//! month it started, and an unfinished run from last month still counts now.

use crate::store::{Result, Store, StoreError};
use decurion_proto::Spend;
use sqlx::{Sqlite, Transaction};

/// Most a single pay-per-use run may spend.
pub const PER_RUN_MAX_MICROS: i64 = 10_000_000;
/// Below this much left in the month, pay-per-use work stops.
pub const MIN_RUN_MICROS: i64 = 500_000;
/// Share of the set-aside money given to Claude Code as its limit; the rest
/// absorbs the last model call going over.
const BUDGET_SHARE: f64 = 0.9;
/// Largest single cost report accepted, in USD. Anything bigger is a bad
/// report and is clamped rather than allowed to overflow the totals.
const MAX_REPORT_USD: f64 = 1_000.0;

/// The `--max-budget-usd` value for a run: earlier session cost plus most of
/// the set-aside money, rounded down to the cent.
pub fn claude_budget_usd(prior_micros: i64, reserved_micros: i64) -> f64 {
    let micros = prior_micros + (reserved_micros as f64 * BUDGET_SHARE) as i64;
    (micros / 10_000) as f64 / 100.0
}

pub fn to_usd(micros: i64) -> f64 {
    micros as f64 / 1_000_000.0
}

pub fn to_micros(usd: f64) -> i64 {
    (usd * 1_000_000.0).round() as i64
}

const MONTH_START: &str = "strftime('%Y-%m-%dT%H:%M:%fZ', 'now', 'start of month')";

pub(crate) async fn cap_micros(tx: &mut Transaction<'_, Sqlite>) -> Result<i64> {
    let v: String =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'monthly_cap_micros'")
            .fetch_one(&mut **tx)
            .await?;
    v.parse()
        .map_err(|_| StoreError::Invalid("bad cap setting".into()))
}

/// Pay-per-use money used this month: real cost where known, otherwise the
/// amount set aside.
pub(crate) async fn api_spent_micros(tx: &mut Transaction<'_, Sqlite>) -> Result<i64> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT COALESCE(SUM(COALESCE(cost_micros, reserved_micros)), 0) FROM runs
         WHERE account = 'api' AND (started_at >= {MONTH_START} OR finished_at IS NULL)"
    ))
    .fetch_one(&mut **tx)
    .await?)
}

/// Starts a run row. For pay-per-use, sets aside up to [`PER_RUN_MAX_MICROS`]
/// of what is left, or returns `None` if the month's cap is used up.
pub(crate) async fn open_run(
    tx: &mut Transaction<'_, Sqlite>,
    thread_id: i64,
    account: &str,
    prior_micros: i64,
) -> Result<Option<i64>> {
    let reserve = if account == "api" {
        let left = cap_micros(tx).await? - api_spent_micros(tx).await?;
        if left < MIN_RUN_MICROS {
            return Ok(None);
        }
        left.min(PER_RUN_MAX_MICROS)
    } else {
        0
    };
    sqlx::query(
        "INSERT INTO runs (thread_id, account, reserved_micros, prior_micros) VALUES (?, ?, ?, ?)",
    )
    .bind(thread_id)
    .bind(account)
    .bind(reserve)
    .bind(prior_micros)
    .execute(&mut **tx)
    .await?;
    Ok(Some(reserve))
}

/// What earlier runs of a thread's current session were charged.
pub(crate) async fn session_cost_micros(
    tx: &mut Transaction<'_, Sqlite>,
    thread_id: i64,
) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(SUM(COALESCE(cost_micros, reserved_micros)), 0) FROM runs WHERE thread_id = ?",
    )
    .bind(thread_id)
    .fetch_one(&mut **tx)
    .await?)
}

/// Closes a thread's open run when it stops running. Without a trusted cost
/// report, the set-aside amount counts as spent if the container started, and
/// nothing if it never did.
pub(crate) async fn close_open_run(tx: &mut Transaction<'_, Sqlite>, thread_id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE runs SET finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
             cost_micros = COALESCE(cost_micros, CASE WHEN EXISTS (
                 SELECT 1 FROM events e WHERE e.thread_id = runs.thread_id
                     AND e.at >= runs.started_at
                     AND json_extract(e.body, '$.kind') IN ('started', 'message', 'tool')
             ) THEN reserved_micros ELSE 0 END)
         WHERE thread_id = ? AND finished_at IS NULL",
    )
    .bind(thread_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Records what Claude Code said a run cost. The report is a running total
/// for the session, so earlier runs' charges are taken off. If it comes in
/// above the amount set aside, the real figure is kept: the month's total
/// must show what was spent.
pub(crate) async fn record_cost(
    tx: &mut Transaction<'_, Sqlite>,
    thread_id: i64,
    cost_usd: f64,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> Result<()> {
    let reported = if cost_usd.is_finite() {
        to_micros(cost_usd.clamp(0.0, MAX_REPORT_USD))
    } else {
        return Ok(());
    };
    sqlx::query(
        "UPDATE runs SET cost_micros = MAX(? - prior_micros, 0), input_tokens = ?, output_tokens = ?
         WHERE id = (SELECT id FROM runs WHERE thread_id = ? AND finished_at IS NULL
                     ORDER BY id DESC LIMIT 1)",
    )
    .bind(reported)
    .bind(input_tokens.map(|n| n as i64))
    .bind(output_tokens.map(|n| n as i64))
    .bind(thread_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

impl Store {
    pub async fn spend(&self) -> Result<Spend> {
        let mut tx = self.pool().begin().await?;
        let cap = cap_micros(&mut tx).await?;
        let api = api_spent_micros(&mut tx).await?;
        let max: i64 = sqlx::query_scalar(&format!(
            "SELECT COALESCE(SUM(cost_micros), 0) FROM runs
             WHERE account IN ('A', 'B') AND started_at >= {MONTH_START}"
        ))
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Spend {
            pay_per_use_usd: to_usd(api),
            cap_usd: to_usd(cap),
            max_equivalent_usd: to_usd(max),
        })
    }

    pub async fn set_cap(&self, usd: f64) -> Result<()> {
        if !(0.0..=100_000.0).contains(&usd) {
            return Err(StoreError::Invalid(
                "cap must be between $0 and $100,000".into(),
            ));
        }
        sqlx::query("UPDATE settings SET value = ? WHERE key = 'monthly_cap_micros'")
            .bind(to_micros(usd).to_string())
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn set_pay_per_use(&self, app_id: i64, on: bool) -> Result<()> {
        let done = sqlx::query("UPDATE apps SET pay_per_use = ? WHERE id = ?")
            .bind(on)
            .bind(app_id)
            .execute(self.pool())
            .await?;
        if done.rows_affected() != 1 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }
}

impl Store {
    /// The account the thread's current run is billed to (`A`, `B` or `api`).
    pub async fn open_run_account(&self, thread_id: i64) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "SELECT account FROM runs WHERE thread_id = ? AND finished_at IS NULL
             ORDER BY id DESC LIMIT 1",
        )
        .bind(thread_id)
        .fetch_optional(self.pool())
        .await?)
    }
}
