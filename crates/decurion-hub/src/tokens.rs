//! Tokens the agents used: recorded per run and per helper job, added up per
//! piece, per goal and per app for today. The goal page and the Today strip
//! show them, so the owner can compare a goal's usage with doing the same by
//! hand, and see how much Claude read back from its prompt cache.
//!
//! Rules that must stay true:
//! - Each row holds its own share. Claude Code reports a resumed session's
//!   tokens as a running total (like its cost), so a run or job marked
//!   `resumed` has the earlier rows of the same session taken off, never
//!   going below zero. pi reports one run at a time and is never `resumed`.
//! - Only what an agent reported counts; a run with no report adds nothing.
//! - This only reads and records counts. It never decides whether work may
//!   run: money stays with `spend.rs`, Max meters with `usage.rs`.

use crate::store::Result;
use decurion_proto::tokens::{TokenLine, TokenUse};
use sqlx::{Row, Sqlite, SqliteConnection, Transaction};

/// A row's columns turned into its share of the session, given what the
/// agent reported (a running total when `resumed`) and the earlier rows'
/// shares.
fn share(reported: &TokenUse, prior: &TokenUse) -> TokenUse {
    TokenUse {
        fresh: reported.fresh.saturating_sub(prior.fresh),
        cache_read: reported.cache_read.saturating_sub(prior.cache_read),
        cache_write: reported.cache_write.saturating_sub(prior.cache_write),
        output: reported.output.saturating_sub(prior.output),
        peak_context: reported.peak_context,
        compactions: reported.compactions,
    }
}

/// Sums of the token columns, from `{table}` rows matching `filter`. Names
/// and filters are Decurion's own, never the owner's or an agent's text.
const SUMS: &str = "COALESCE(SUM(MAX(COALESCE(input_tokens, 0) - COALESCE(cache_read_tokens, 0)
                        - COALESCE(cache_write_tokens, 0), 0)), 0) AS fresh,
                    COALESCE(SUM(cache_read_tokens), 0) AS cache_read,
                    COALESCE(SUM(cache_write_tokens), 0) AS cache_write,
                    COALESCE(SUM(output_tokens), 0) AS output,
                    COALESCE(MAX(peak_context_tokens), 0) AS peak,
                    COALESCE(SUM(compactions), 0) AS compactions";

fn from_sums(r: &sqlx::sqlite::SqliteRow) -> Result<TokenUse> {
    let n = |k: &str| -> Result<u64> { Ok(r.try_get::<i64, _>(k)?.max(0) as u64) };
    Ok(TokenUse {
        fresh: n("fresh")?,
        cache_read: n("cache_read")?,
        cache_write: n("cache_write")?,
        output: n("output")?,
        peak_context: n("peak")?,
        compactions: n("compactions")?,
    })
}

fn bind_use<'q>(
    q: sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    t: &TokenUse,
) -> sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    let i = |n: u64| n.min(i64::MAX as u64) as i64;
    q.bind(i(t.input()))
        .bind(i(t.output))
        .bind(i(t.cache_read))
        .bind(i(t.cache_write))
        .bind(i(t.peak_context))
        .bind(i(t.compactions))
}

/// Records a thread's open run's tokens from a `Finished` report. Without a
/// split (an older agent), the plain counts are kept as they came.
pub(crate) async fn record_run(
    tx: &mut Transaction<'_, Sqlite>,
    thread_id: i64,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    tokens: Option<&TokenUse>,
) -> Result<()> {
    let Some((run_id, resumed)) = sqlx::query_as::<_, (i64, bool)>(
        "SELECT id, resumed FROM runs WHERE thread_id = ? AND finished_at IS NULL
         ORDER BY id DESC LIMIT 1",
    )
    .bind(thread_id)
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(());
    };
    let Some(reported) = tokens else {
        sqlx::query("UPDATE runs SET input_tokens = ?, output_tokens = ? WHERE id = ?")
            .bind(input_tokens.map(|n| n as i64))
            .bind(output_tokens.map(|n| n as i64))
            .bind(run_id)
            .execute(&mut **tx)
            .await?;
        return Ok(());
    };
    let prior = if resumed {
        // The session's earlier runs: back to the newest run that started it.
        let r = sqlx::query(&format!(
            "SELECT {SUMS} FROM runs WHERE thread_id = ?1 AND id < ?2
               AND id >= COALESCE((SELECT MAX(id) FROM runs
                                   WHERE thread_id = ?1 AND id < ?2 AND resumed = 0), 0)"
        ))
        .bind(thread_id)
        .bind(run_id)
        .fetch_one(&mut **tx)
        .await?;
        from_sums(&r)?
    } else {
        TokenUse::default()
    };
    let own = share(reported, &prior);
    bind_use(
        sqlx::query(
            "UPDATE runs SET input_tokens = ?, output_tokens = ?, cache_read_tokens = ?,
                 cache_write_tokens = ?, peak_context_tokens = ?, compactions = ?
             WHERE id = ?",
        ),
        &own,
    )
    .bind(run_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Records a helper job's tokens as it closes. A resumed project-lead
/// session has its earlier jobs' share taken off, as `prior_micros` does.
pub(crate) async fn record_job(
    tx: &mut Transaction<'_, Sqlite>,
    job_id: i64,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    tokens: Option<&TokenUse>,
) -> Result<()> {
    let Some(reported) = tokens else {
        sqlx::query("UPDATE helper_jobs SET input_tokens = ?, output_tokens = ? WHERE id = ?")
            .bind(input_tokens.map(|n| n as i64))
            .bind(output_tokens.map(|n| n as i64))
            .bind(job_id)
            .execute(&mut **tx)
            .await?;
        return Ok(());
    };
    let r = sqlx::query(&format!(
        "SELECT {SUMS} FROM helper_jobs WHERE id IN (
             SELECT e.id FROM helper_jobs e JOIN helper_jobs j ON j.id = ?1
             WHERE j.resumed = 1 AND e.goal_id = j.goal_id AND e.kind = 'coordinator'
               AND e.session_id = j.session_id AND e.id < j.id
               AND e.state IN ('done', 'failed'))"
    ))
    .bind(job_id)
    .fetch_one(&mut **tx)
    .await?;
    let own = share(reported, &from_sums(&r)?);
    bind_use(
        sqlx::query(
            "UPDATE helper_jobs SET input_tokens = ?, output_tokens = ?, cache_read_tokens = ?,
                 cache_write_tokens = ?, peak_context_tokens = ?, compactions = ?
             WHERE id = ?",
        ),
        &own,
    )
    .bind(job_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A total worth showing: `None` when nothing was reported.
fn line(t: TokenUse) -> Option<TokenLine> {
    (!t.is_empty()).then(|| t.into())
}

/// One piece's tokens: its runs and its checks.
pub(crate) async fn piece(conn: &mut SqliteConnection, thread_id: i64) -> Result<TokenUse> {
    let mut t = from_sums(
        &sqlx::query(&format!("SELECT {SUMS} FROM runs WHERE thread_id = ?"))
            .bind(thread_id)
            .fetch_one(&mut *conn)
            .await?,
    )?;
    t.add(&from_sums(
        &sqlx::query(&format!(
            "SELECT {SUMS} FROM helper_jobs WHERE thread_id = ?"
        ))
        .bind(thread_id)
        .fetch_one(&mut *conn)
        .await?,
    )?);
    Ok(t)
}

pub(crate) fn piece_line(t: TokenUse) -> Option<TokenLine> {
    line(t)
}

/// A goal's tokens: the project lead's jobs, and the pieces' totals given.
pub(crate) async fn goal(
    conn: &mut SqliteConnection,
    goal_id: i64,
    pieces: &[TokenUse],
) -> Result<Option<decurion_proto::goal::GoalUsage>> {
    let lead = from_sums(
        &sqlx::query(&format!(
            "SELECT {SUMS} FROM helper_jobs WHERE goal_id = ? AND thread_id IS NULL"
        ))
        .bind(goal_id)
        .fetch_one(&mut *conn)
        .await?,
    )?;
    let mut work = TokenUse::default();
    pieces.iter().for_each(|p| work.add(p));
    let mut total = lead;
    total.add(&work);
    Ok(
        (!total.is_empty()).then(|| decurion_proto::goal::GoalUsage {
            total: total.into(),
            lead: lead.into(),
            pieces: work.into(),
        }),
    )
}

/// Each app's tokens for today, on the hub's own clock: runs of its work and
/// helper jobs of its goals that started today.
pub(crate) async fn today_by_app(
    pool: &sqlx::SqlitePool,
) -> Result<std::collections::HashMap<i64, TokenLine>> {
    const TODAY: &str = "date(x.started_at, 'localtime') = date('now', 'localtime')";
    let mut by_app: std::collections::HashMap<i64, TokenUse> = Default::default();
    for sql in [
        format!(
            "SELECT t.app_id, {SUMS} FROM runs x JOIN threads t ON t.id = x.thread_id
             WHERE {TODAY} GROUP BY t.app_id"
        ),
        format!(
            "SELECT g.app_id, {SUMS} FROM helper_jobs x JOIN goals g ON g.id = x.goal_id
             WHERE x.started_at IS NOT NULL AND {TODAY} GROUP BY g.app_id"
        ),
    ] {
        for r in sqlx::query(&sql).fetch_all(pool).await? {
            by_app
                .entry(r.try_get("app_id")?)
                .or_default()
                .add(&from_sums(&r)?);
        }
    }
    Ok(by_app
        .into_iter()
        .filter_map(|(app, t)| line(t).map(|l| (app, l)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resumed_rows_share_never_goes_below_zero() {
        let reported = TokenUse {
            fresh: 10,
            cache_read: 500,
            output: 5,
            peak_context: 300,
            compactions: 1,
            ..TokenUse::default()
        };
        let prior = TokenUse {
            fresh: 20,
            cache_read: 200,
            output: 2,
            ..TokenUse::default()
        };
        let s = share(&reported, &prior);
        assert_eq!((s.fresh, s.cache_read, s.output), (0, 300, 3));
        // The peak and compactions are the run's own, never subtracted.
        assert_eq!((s.peak_context, s.compactions), (300, 1));
    }
}
