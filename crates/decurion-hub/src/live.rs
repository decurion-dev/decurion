//! What is alive right now, for the screens: the newest step or note each
//! piece of work reported, and the helper agents (checkers, project leads,
//! visual plans being drawn) that are working or lined up.
//!
//! The board counted only building agents, so a check that ran for twenty
//! minutes showed as "No agents working right now", and a piece waiting for
//! its check offered an Accept that would have skipped it. These reads fill
//! that in. They only describe; nothing here moves work, starts a job or
//! decides anything, and the claim (`claim_helper` in `coordinator.rs`)
//! stays the one place that says what runs.
//!
//! Rules:
//! - A check is "coming" while its piece sits in review with its check not
//!   yet settled and its goal still open: the same pieces `claim_helper`
//!   checks (it queues a missed one itself), so a stopped or closed goal
//!   never leaves a check showing.
//! - A queued job says why it waits only for what the claim itself skips it
//!   for: an app on a Max plan that is out of usage, with no pay-per-use tap
//!   for this outage. Otherwise it waits for a free agent.
//! - The agent's note is its own words, clipped, and given only while the
//!   work runs. It is text for the page to show as text.

use crate::limits::PAY_PER_USE_NOW;
use crate::store::{thread_from, Result, Store, StoreError, THREAD_COLS};
use crate::{forbidden, ApiError, AppState, Caller};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use decurion_proto::goal::{CheckNow, JobNow};
use decurion_proto::Thread;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, Transaction};
use std::collections::HashMap;

/// How much of the agent's newest note a card gets.
const NOTE_CHARS: usize = 280;

/// A piece of work as the screens read it: the work itself, and what is
/// alive about it now.
#[derive(Debug, Clone, Serialize)]
pub struct ThreadView {
    #[serde(flatten)]
    pub thread: Thread,
    /// When the work last did anything: its newest step or note, or its
    /// last change of state if that came later. "Last activity" on screens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
    /// While it runs: the newest thing the agent said in its own words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_note: Option<Note>,
    /// For a piece of a goal: the check coming for it, while one is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckNow>,
}

/// Something the agent said, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub text: String,
    pub at: String,
}

/// A helper agent's job that has not finished, for the board's Running and
/// Queued columns.
#[derive(Debug, Clone, Serialize)]
pub struct HelperNow {
    pub job_id: i64,
    /// `checker`, `project_lead` or `designer`.
    pub kind: &'static str,
    pub goal_id: i64,
    pub goal_title: String,
    pub app_id: i64,
    /// The piece a checker checks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub piece_title: Option<String>,
    #[serde(flatten)]
    pub now: CheckNow,
}

/// The end of the outage holding up the app's Claude helpers, if any: the
/// claim's own rule (an app on pay-per-use never waits; a Max plan out of
/// usage waits unless the owner chose pay-per-use until its reset). Needs
/// `apps a`.
fn out_until() -> String {
    format!(
        "CASE WHEN a.pay_per_use = 0 THEN (SELECT l.until FROM profile_limits l
             WHERE l.profile = a.profile AND l.until > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
               AND NOT {PAY_PER_USE_NOW}) END"
    )
}

/// The newest step and, while it runs, the newest note of each thread.
/// Needs `threads`; both are one indexed look back per thread.
const LIVE_COLS: &str = "(SELECT e.at FROM events e WHERE e.thread_id = threads.id
         ORDER BY e.id DESC LIMIT 1) AS last_event_at,
     CASE WHEN threads.state = 'running' THEN (SELECT json_object('text', json_extract(e.body, '$.text'), 'at', e.at)
         FROM events e WHERE e.thread_id = threads.id AND json_extract(e.body, '$.kind') = 'message'
           AND TRIM(COALESCE(json_extract(e.body, '$.text'), '')) <> ''
         ORDER BY e.id DESC LIMIT 1) END AS last_note";

/// A job's state as the screens show it. `out` is the end of a Max-plan
/// outage holding it up, for a queued job; `profile` names the plan.
fn now_from(
    state: &str,
    started_at: Option<String>,
    profile: &str,
    out: Option<String>,
) -> CheckNow {
    if state == "running" {
        return CheckNow {
            state: JobNow::Running,
            started_at,
            waiting: None,
            until: None,
        };
    }
    CheckNow {
        state: JobNow::Queued,
        started_at: None,
        waiting: out
            .as_ref()
            .map(|_| format!("Claude Max plan {profile} is out of usage for now.")),
        until: out,
    }
}

/// The note as one short line: whitespace folded, clipped on a character.
fn short(text: &str) -> String {
    let one = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= NOTE_CHARS {
        return one;
    }
    let mut cut: String = one.chars().take(NOTE_CHARS).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

fn view_from(row: &SqliteRow, checks: &HashMap<i64, CheckNow>) -> Result<ThreadView> {
    let thread = thread_from(row)?;
    let event: Option<String> = row.try_get("last_event_at")?;
    let last_activity_at = Some(match event {
        Some(e) if e > thread.updated_at => e,
        _ => thread.updated_at.clone(),
    });
    let last_note = row
        .try_get::<Option<String>, _>("last_note")?
        .and_then(|j| serde_json::from_str::<Note>(&j).ok())
        .map(|n| Note {
            text: short(&n.text),
            at: n.at,
        });
    let check = checks.get(&thread.id).cloned();
    Ok(ThreadView {
        thread,
        last_activity_at,
        last_note,
        check,
    })
}

/// The checks coming for pieces in review, by thread; only `thread`'s when
/// given. A piece in review whose check is not queued yet (the hub queues a
/// missed one at the next claim) shows as lined up.
pub(crate) async fn checks(
    tx: &mut Transaction<'_, Sqlite>,
    thread: Option<i64>,
) -> Result<HashMap<i64, CheckNow>> {
    let rows = sqlx::query(&format!(
        "SELECT p.thread_id, j.state AS job_state, j.started_at, a.profile, {} AS out_until
         FROM goal_pieces p JOIN threads t ON t.id = p.thread_id JOIN goals g ON g.id = p.goal_id
         JOIN apps a ON a.id = g.app_id
         LEFT JOIN helper_jobs j ON j.id = (SELECT MAX(r.id) FROM helper_jobs r
             WHERE r.thread_id = p.thread_id AND r.kind = 'review' AND r.state IN ('queued', 'running'))
         WHERE t.state = 'in_review' AND t.research = 0
           AND p.review IN ('building', 'reviewing', 'fixing')
           AND g.state NOT IN ('done', 'stopped')
           AND (?1 IS NULL OR p.thread_id = ?1)",
        out_until()
    ))
    .bind(thread)
    .fetch_all(&mut **tx)
    .await?;
    let mut out = HashMap::new();
    for r in &rows {
        let state: Option<String> = r.try_get("job_state")?;
        let profile: String = r.try_get("profile")?;
        out.insert(
            r.try_get("thread_id")?,
            now_from(
                state.as_deref().unwrap_or("queued"),
                r.try_get("started_at")?,
                &profile,
                r.try_get("out_until")?,
            ),
        );
    }
    Ok(out)
}

impl Store {
    /// Every piece of work, with what is alive about it now.
    pub async fn thread_views(&self) -> Result<Vec<ThreadView>> {
        let mut tx = self.pool().begin().await?; // read-only
        let checks = checks(&mut tx, None).await?;
        let rows = sqlx::query(&format!(
            "SELECT {THREAD_COLS}, {LIVE_COLS} FROM threads ORDER BY id"
        ))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut views = rows
            .iter()
            .map(|r| view_from(r, &checks))
            .collect::<Result<Vec<_>>>()?;
        // Asked after the read: it may ask an app's clone (read-only).
        self.with_roads(views.iter_mut().map(|v| &mut v.thread))
            .await?;
        Ok(views)
    }

    /// One piece of work, with what is alive about it now.
    pub async fn thread_view(&self, id: i64) -> Result<ThreadView> {
        let mut tx = self.pool().begin().await?; // read-only
        let checks = checks(&mut tx, Some(id)).await?;
        let row = sqlx::query(&format!(
            "SELECT {THREAD_COLS}, {LIVE_COLS} FROM threads WHERE id = ?"
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::NotFound)?;
        tx.commit().await?;
        let mut view = view_from(&row, &checks)?;
        self.with_roads([&mut view.thread]).await?;
        Ok(view)
    }

    /// Helper agents' jobs running now or lined up, oldest first. A job
    /// still running for a goal that has since closed is counted while it
    /// runs: it holds an agent until it ends.
    pub async fn helpers_now(&self) -> Result<Vec<HelperNow>> {
        let rows = sqlx::query(&format!(
            "SELECT j.id, j.kind, j.goal_id, j.thread_id, j.state, j.started_at,
                    g.title AS goal_title, g.app_id, p.title AS piece_title, a.profile,
                    {} AS out_until
             FROM helper_jobs j JOIN goals g ON g.id = j.goal_id JOIN apps a ON a.id = g.app_id
             LEFT JOIN goal_pieces p ON p.thread_id = j.thread_id
             WHERE j.state = 'running' OR (j.state = 'queued' AND g.state NOT IN ('done', 'stopped'))
             ORDER BY j.id",
            out_until()
        ))
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                let kind: String = r.try_get("kind")?;
                let state: String = r.try_get("state")?;
                let profile: String = r.try_get("profile")?;
                Ok(HelperNow {
                    job_id: r.try_get("id")?,
                    kind: match kind.as_str() {
                        "review" => "checker",
                        "design" => "designer",
                        _ => "project_lead",
                    },
                    goal_id: r.try_get("goal_id")?,
                    goal_title: r.try_get("goal_title")?,
                    app_id: r.try_get("app_id")?,
                    thread_id: r.try_get("thread_id")?,
                    piece_title: r.try_get("piece_title")?,
                    now: now_from(
                        &state,
                        r.try_get("started_at")?,
                        &profile,
                        r.try_get("out_until")?,
                    ),
                })
            })
            .collect()
    }
}

/// `GET /api/helpers`: the owner's board. Owner-only: it names goals and
/// pieces across every app.
pub(crate) async fn list(State(st): State<AppState>, Extension(c): Extension<Caller>) -> Response {
    if c != Caller::Owner {
        return forbidden("seeing what helper agents are doing");
    }
    match st.store.helpers_now().await {
        Ok(v) => Json(v).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_note_becomes_one_short_line() {
        assert_eq!(
            short("Build and tests\n\n  are running."),
            "Build and tests are running."
        );
        let long = "word ".repeat(100);
        let s = short(&long);
        assert!(
            s.ends_with('…') && s.chars().count() <= NOTE_CHARS + 1,
            "{s}"
        );
    }

    #[test]
    fn only_a_queued_job_on_a_plan_that_is_out_says_why_it_waits() {
        let q = now_from("queued", None, "A", Some("2026-10-19T16:30:00.000Z".into()));
        assert_eq!(q.state, JobNow::Queued);
        assert_eq!(
            q.waiting.as_deref(),
            Some("Claude Max plan A is out of usage for now.")
        );
        let free = now_from("queued", None, "A", None);
        assert_eq!((free.waiting, free.until), (None, None));
        let r = now_from("running", Some("t".into()), "A", Some("x".into()));
        assert_eq!(r.state, JobNow::Running);
        assert_eq!(r.started_at.as_deref(), Some("t"));
        assert_eq!(r.waiting, None);
    }
}
