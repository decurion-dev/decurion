//! Money. Every run gets a row in `runs`. A pay-per-use run sets money aside
//! from the monthly cap before it starts, and Claude Code is told that amount
//! as its own hard limit (`--max-budget-usd`), so a run cannot spend more than
//! was set aside and the month cannot go over the cap by more than rounding.
//!
//! When a run's real cost is unknown (the runner died, or the agent never
//! reported), the hub assumes the whole set-aside amount was spent if the
//! agent did anything at all. Erring high keeps the cap a real limit.

use crate::store::{Result, Store, StoreError};
use decurion_proto::Spend;
use sqlx::{Sqlite, Transaction};

/// Most a single pay-per-use run may spend.
pub const PER_RUN_MAX_MICROS: i64 = 10_000_000;
/// Below this much left in the month, pay-per-use work stops.
pub const MIN_RUN_MICROS: i64 = 500_000;

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
         WHERE account = 'api' AND started_at >= {MONTH_START}"
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
    sqlx::query("INSERT INTO runs (thread_id, account, reserved_micros) VALUES (?, ?, ?)")
        .bind(thread_id)
        .bind(account)
        .bind(reserve)
        .execute(&mut **tx)
        .await?;
    Ok(Some(reserve))
}

/// Closes a thread's open run when it stops running. If the agent never
/// reported a cost, the set-aside amount counts as spent when the agent did
/// anything during the run, and nothing when it never got going.
pub(crate) async fn close_open_run(tx: &mut Transaction<'_, Sqlite>, thread_id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE runs SET finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
             cost_micros = COALESCE(cost_micros, CASE WHEN EXISTS (
                 SELECT 1 FROM events e WHERE e.thread_id = runs.thread_id
                     AND e.at >= runs.started_at
                     AND json_extract(e.body, '$.kind') IN ('message', 'tool')
             ) THEN reserved_micros ELSE 0 END)
         WHERE thread_id = ? AND finished_at IS NULL",
    )
    .bind(thread_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Records what Claude Code said a run cost, as reported: if it ever came in
/// above the amount set aside, the month's total must show the real figure.
pub(crate) async fn record_cost(
    tx: &mut Transaction<'_, Sqlite>,
    thread_id: i64,
    cost_usd: f64,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> Result<()> {
    let cost = to_micros(cost_usd.max(0.0));
    sqlx::query(
        "UPDATE runs SET cost_micros = ?, input_tokens = ?, output_tokens = ?
         WHERE id = (SELECT id FROM runs WHERE thread_id = ? AND finished_at IS NULL
                     ORDER BY id DESC LIMIT 1)",
    )
    .bind(cost)
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
