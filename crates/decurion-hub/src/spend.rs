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
use decurion_proto::{Account, AccountMeter, AppSpend, Spend};
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

/// The settings key holding a paid account's monthly cap. The Anthropic key's
/// predates the others and keeps its original name.
fn cap_key(account: Account) -> String {
    match account {
        Account::Api => "monthly_cap_micros".to_owned(),
        other => format!("monthly_cap_micros:{}", other.as_str()),
    }
}

/// A paid account's monthly cap.
pub(crate) async fn cap_micros(tx: &mut Transaction<'_, Sqlite>, account: Account) -> Result<i64> {
    let v: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(cap_key(account))
        .fetch_one(&mut **tx)
        .await?;
    v.parse()
        .map_err(|_| StoreError::Invalid("bad cap setting".into()))
}

/// Money an account used this month: real cost where known, otherwise the
/// amount set aside. Helper jobs (coordinator, designer, checker) count too.
pub(crate) async fn spent_micros(
    tx: &mut Transaction<'_, Sqlite>,
    account: Account,
) -> Result<i64> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT (SELECT COALESCE(SUM(COALESCE(cost_micros, reserved_micros)), 0) FROM runs
                 WHERE account = ?1 AND (started_at >= {MONTH_START} OR finished_at IS NULL))
              + (SELECT COALESCE(SUM(COALESCE(cost_micros, reserved_micros)), 0) FROM helper_jobs
                 WHERE account = ?1 AND started_at IS NOT NULL
                   AND (started_at >= {MONTH_START} OR finished_at IS NULL))"
    ))
    .bind(account.as_str())
    .fetch_one(&mut **tx)
    .await?)
}

/// Pay-per-use money an app's work used this month, across paid accounts,
/// helper jobs for its goals included.
pub(crate) async fn app_spent_micros(tx: &mut Transaction<'_, Sqlite>, app_id: i64) -> Result<i64> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT (SELECT COALESCE(SUM(COALESCE(r.cost_micros, r.reserved_micros)), 0)
                 FROM runs r JOIN threads t ON t.id = r.thread_id
                 WHERE t.app_id = ?1 AND r.account NOT IN ('A', 'B')
                   AND (r.started_at >= {MONTH_START} OR r.finished_at IS NULL))
              + (SELECT COALESCE(SUM(COALESCE(h.cost_micros, h.reserved_micros)), 0)
                 FROM helper_jobs h JOIN goals g ON g.id = h.goal_id
                 WHERE g.app_id = ?1 AND h.account NOT IN ('A', 'B') AND h.started_at IS NOT NULL
                   AND (h.started_at >= {MONTH_START} OR h.finished_at IS NULL))"
    ))
    .bind(app_id)
    .fetch_one(&mut **tx)
    .await?)
}

/// Why a paid run could not start.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CapHit {
    /// The account's monthly cap, in USD.
    Account(Account, f64),
    /// The app's own monthly cap, in USD.
    App(f64),
}

impl CapHit {
    /// The note on the card and in the audit log.
    pub fn note(self) -> String {
        match self {
            CapHit::Account(Account::Api, cap) => {
                format!("monthly pay-per-use cap of ${cap:.2} reached")
            }
            CapHit::Account(a, cap) => format!("{} monthly cap of ${cap:.2} reached", a.label()),
            CapHit::App(cap) => format!("this app's monthly cap of ${cap:.2} reached"),
        }
    }
}

/// How much to set aside for one run or helper job of an app on a paid
/// account: up to [`PER_RUN_MAX_MICROS`] of what is left under both the
/// account's cap and the app's own cap, or which cap is used up. Nothing for
/// a Max account.
pub(crate) async fn set_aside(
    tx: &mut Transaction<'_, Sqlite>,
    app_id: i64,
    account: Account,
) -> Result<std::result::Result<i64, CapHit>> {
    if !account.is_paid() {
        return Ok(Ok(0));
    }
    let cap = cap_micros(tx, account).await?;
    let left = cap - spent_micros(tx, account).await?;
    if left < MIN_RUN_MICROS {
        return Ok(Err(CapHit::Account(account, to_usd(cap))));
    }
    let app_cap: Option<i64> =
        sqlx::query_scalar("SELECT monthly_cap_micros FROM apps WHERE id = ?")
            .bind(app_id)
            .fetch_one(&mut **tx)
            .await?;
    let left = match app_cap {
        Some(app_cap) => {
            let app_left = app_cap - app_spent_micros(tx, app_id).await?;
            if app_left < MIN_RUN_MICROS {
                return Ok(Err(CapHit::App(to_usd(app_cap))));
            }
            left.min(app_left)
        }
        None => left,
    };
    Ok(Ok(left.min(PER_RUN_MAX_MICROS)))
}

/// Starts a run row, with the money [`set_aside`] for it, or says which cap
/// is used up.
pub(crate) async fn open_run(
    tx: &mut Transaction<'_, Sqlite>,
    thread_id: i64,
    app_id: i64,
    account: Account,
    prior_micros: i64,
) -> Result<std::result::Result<i64, CapHit>> {
    let reserve = match set_aside(tx, app_id, account).await? {
        Ok(r) => r,
        Err(hit) => return Ok(Err(hit)),
    };
    sqlx::query(
        "INSERT INTO runs (thread_id, account, reserved_micros, prior_micros) VALUES (?, ?, ?, ?)",
    )
    .bind(thread_id)
    .bind(account.as_str())
    .bind(reserve)
    .bind(prior_micros)
    .execute(&mut **tx)
    .await?;
    Ok(Ok(reserve))
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
                     AND json_extract(e.body, '$.kind') IN ('started', 'message', 'tool', 'blocked')
             ) THEN reserved_micros ELSE 0 END)
         WHERE thread_id = ? AND finished_at IS NULL",
    )
    .bind(thread_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Records what the agent said a run cost. Claude Code's report is a running
/// total for the session, so earlier runs' charges are taken off; a pi run's
/// row has no earlier charges, because pi reports only the run. If it comes in
/// above the amount set aside, the real figure is kept: the month's total
/// must show what was spent.
pub(crate) async fn record_cost(
    tx: &mut Transaction<'_, Sqlite>,
    thread_id: i64,
    cost_usd: f64,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> Result<()> {
    // The report is a running total for the session; the limit applies to
    // this run's own share of it.
    let reported = if cost_usd.is_finite() {
        to_micros(cost_usd.clamp(0.0, 1e9))
    } else {
        return Ok(());
    };
    sqlx::query(
        "UPDATE runs SET cost_micros = MIN(MAX(?1 - prior_micros, 0), ?2),
             input_tokens = ?3, output_tokens = ?4
         WHERE id = (SELECT id FROM runs WHERE thread_id = ?5 AND finished_at IS NULL
                     ORDER BY id DESC LIMIT 1)",
    )
    .bind(reported)
    .bind(to_micros(MAX_REPORT_USD))
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
        let cap = cap_micros(&mut tx, Account::Api).await?;
        let api = spent_micros(&mut tx, Account::Api).await?;
        let max: i64 = sqlx::query_scalar(&format!(
            "SELECT (SELECT COALESCE(SUM(cost_micros), 0) FROM runs
                     WHERE account IN ('A', 'B') AND started_at >= {MONTH_START})
                  + (SELECT COALESCE(SUM(cost_micros), 0) FROM helper_jobs
                     WHERE account IN ('A', 'B') AND started_at >= {MONTH_START})"
        ))
        .fetch_one(&mut *tx)
        .await?;
        let app_caps: Vec<(i64, Option<i64>)> =
            sqlx::query_as("SELECT id, monthly_cap_micros FROM apps ORDER BY name")
                .fetch_all(&mut *tx)
                .await?;
        let mut apps = Vec::with_capacity(app_caps.len());
        for (app_id, cap) in app_caps {
            apps.push(AppSpend {
                app_id,
                spent_usd: to_usd(app_spent_micros(&mut tx, app_id).await?),
                cap_usd: cap.map(to_usd),
            });
        }
        tx.commit().await?;
        Ok(Spend {
            pay_per_use_usd: to_usd(api),
            cap_usd: to_usd(cap),
            max_equivalent_usd: to_usd(max),
            apps,
        })
    }

    /// Every account, in the order the board shows them. Paid accounts carry
    /// this month's money; Max plans carry what Claude last reported.
    pub async fn accounts(&self) -> Result<Vec<AccountMeter>> {
        let mut tx = self.pool().begin().await?;
        let mut out = Vec::with_capacity(Account::ALL.len());
        for account in Account::ALL {
            let (spent_usd, cap_usd) = if account.is_paid() {
                (
                    Some(to_usd(spent_micros(&mut tx, account).await?)),
                    Some(to_usd(cap_micros(&mut tx, account).await?)),
                )
            } else {
                (None, None)
            };
            let mut meter = AccountMeter {
                account,
                label: account.label().to_owned(),
                spent_usd,
                cap_usd,
                week_pct: None,
                five_hour_pct: None,
                resets_at: None,
                week_resets_at: None,
                five_hour_resets_at: None,
                limited: false,
                seen_at: None,
            };
            if !account.is_paid() {
                crate::usage::fill_meter(&mut tx, &mut meter).await?;
            }
            out.push(meter);
        }
        tx.commit().await?;
        Ok(out)
    }

    /// Sets a paid account's monthly cap.
    pub async fn set_cap(&self, account: Account, usd: f64) -> Result<()> {
        if !account.is_paid() {
            return Err(StoreError::Invalid(
                "Max plans are flat-rate and have no cap".into(),
            ));
        }
        check_cap(usd)?;
        sqlx::query("UPDATE settings SET value = ? WHERE key = ?")
            .bind(to_micros(usd).to_string())
            .bind(cap_key(account))
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Sets or clears an app's own monthly cap.
    pub async fn set_app_cap(&self, app_id: i64, usd: Option<f64>) -> Result<()> {
        if let Some(usd) = usd {
            check_cap(usd)?;
        }
        let done = sqlx::query("UPDATE apps SET monthly_cap_micros = ? WHERE id = ?")
            .bind(usd.map(to_micros))
            .bind(app_id)
            .execute(self.pool())
            .await?;
        if done.rows_affected() != 1 {
            return Err(StoreError::NotFound);
        }
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

fn check_cap(usd: f64) -> Result<()> {
    if (0.0..=100_000.0).contains(&usd) {
        Ok(())
    } else {
        Err(StoreError::Invalid(
            "cap must be between $0 and $100,000".into(),
        ))
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
