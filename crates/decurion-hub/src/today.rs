//! The Today strip: one row per app saying, at a glance, what that app waits
//! on. The board and the phone both draw it from `GET /api/today`.
//!
//! Every word a row shows (its `status`, its `line`) is chosen here, in one
//! place, so the PC and the phone always say the same thing. The pages only
//! pick a colour from the row's `tone`.
//!
//! It also owns the one meaning of "needs you" ([`Store::needs_you`],
//! `GET /api/needs-you`): the app rows, the board's Needs you column and the
//! phone's inbox all count exactly that list, so the three never disagree.
//!
//! This only reads. Watched pull requests and their comments are written by
//! the code that keeps an eye on GitHub; a fix of comments starts from its own
//! route. The routes are owner-only and not in `cloud_reach`: a runner has no
//! business knowing what the owner's other apps are doing.

use crate::store::{Result, Store};
use crate::{forbidden, ApiError, AppState, Caller};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use decurion_proto::goal::GoalState;
use serde::Serialize;
use sqlx::Row;
use std::collections::HashMap;

/// Which colour a row gets, and the order rows come in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    /// Something only the owner can do: answer, approve, check, or fix
    /// review comments.
    NeedsYou,
    /// Agents are building, or the project lead is thinking.
    Working,
    /// A pull request is open and the app's reviewer has it.
    Reviewer,
    /// A pull request is open and the app names no reviewer, so merging it
    /// is the owner's: on GitHub, or with the Merge tap where the app's rules
    /// allow it (nothing merges by itself).
    Merge,
    /// The last pull request was merged.
    Done,
    /// Nothing is going on.
    Quiet,
}

impl Tone {
    /// Rows that need the owner come first, then running ones; the rest
    /// share one place and go by name.
    fn rank(self) -> u8 {
        match self {
            Tone::NeedsYou => 0,
            Tone::Working => 1,
            _ => 2,
        }
    }
}

/// How far the newest working goal has got.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pieces {
    pub goal_id: i64,
    /// Pieces whose building has finished: accepted, or passed (or sent to
    /// the owner by) the checker.
    pub done: i64,
    pub total: i64,
}

/// The app's newest watched pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LastPr {
    /// The work that opened it.
    pub thread_id: i64,
    pub number: i64,
    pub url: String,
    /// `open`, `merged` or `closed`.
    pub state: String,
    /// Plain words for the last thing that happened to it, once known.
    pub event: Option<String>,
    pub event_at: Option<String>,
    /// Still open, and the app's rules let the owner merge it from Decurion
    /// (`POST /api/threads/{id}/pr/merge`).
    pub can_merge: bool,
    /// "Conflicts with main" or "Behind main", while GitHub says so about
    /// the commit Decurion last pushed (see [`crate::pr_watch::health_words`]).
    pub health: Option<String>,
    /// Still open with no fix or update under way: Bring up to date can
    /// start (`POST /api/threads/{id}/pr/update`).
    pub can_update: bool,
}

/// One app's row.
#[derive(Debug, Clone, Serialize)]
pub struct TodayRow {
    pub app_id: i64,
    pub name: String,
    /// The owner starred it to go first in the queue today.
    pub first_today: bool,
    /// Work in state running.
    pub running: i64,
    /// Work waiting for a computer to start it.
    pub queued: i64,
    /// What waits on the owner in this app: its share of
    /// [`Store::needs_you`], the one list the board and the phone count too.
    pub needs_you: i64,
    /// Progress of the newest working goal, when there is one.
    pub pieces: Option<Pieces>,
    /// The GitHub login who reviews this app's pull requests.
    pub reviewer: Option<String>,
    /// Open pull requests Decurion watches for this app. They wait on the
    /// reviewer, or on the owner to merge when the app names none.
    pub waiting_on_reviewer: i64,
    /// Review comments on those open pull requests that no fix was started
    /// for yet.
    pub open_comments: i64,
    /// The work whose pull request has the newest of those comments: the one
    /// "Answer the comments" answers (`POST /api/threads/{id}/pr/fix`).
    pub fix_thread_id: Option<i64>,
    pub last_pr: Option<LastPr>,
    /// The app's deploy card, when its rules allow deploys and there is one
    /// to show: ready, running, failed, or live in the last day.
    pub deploy: Option<crate::deploy::Card>,
    /// The app's rules allow deploys, so Check for changes can show even
    /// when there is no card.
    pub can_deploy: bool,
    pub tone: Tone,
    /// One or two words for the label: "Needs you", "Working", "Done".
    pub status: String,
    /// What the app waits on, in a few plain words: "Approve the plan",
    /// "2 of 4 pieces built", "PR #88 · param left 3 comments".
    pub line: String,
    /// Tokens the app's agents used today, once any did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_today: Option<decurion_proto::tokens::TokenLine>,
}

/// Everything only the owner can move on right now, in one list. The app
/// rows (`needs_you`), the board's Needs you column and the phone's inbox
/// all count this, so they always say the same number.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NeedsYou {
    /// Work that asks the owner something, is ready for them to check (in
    /// review, and no checker is still coming), could not finish, or was
    /// accepted but did not reach GitHub. Work the owner dismissed is left
    /// out.
    pub work: Vec<WorkWait>,
    /// Goals that wait on the owner.
    pub goals: Vec<GoalWait>,
    /// Open pull requests with review comments to answer, or waiting for the
    /// owner's Merge tap, by the work that opened them. Not while a fix of
    /// their comments is under way.
    pub prs: Vec<PrWait>,
    /// How many of all three each app has.
    pub by_app: std::collections::BTreeMap<i64, i64>,
    pub total: i64,
}

/// One piece of work in [`NeedsYou`].
#[derive(Debug, Clone, Serialize)]
pub struct WorkWait {
    pub thread_id: i64,
    pub app_id: i64,
    /// `answer`, `check`, `failed` or `not_sent`.
    pub why: &'static str,
}

/// One goal in [`NeedsYou`].
#[derive(Debug, Clone, Serialize)]
pub struct GoalWait {
    pub goal_id: i64,
    pub app_id: i64,
    /// `plan` (approve it), `answer` (the project lead asked or said
    /// something), `review` (its work is built and checked) or `stuck`.
    pub why: &'static str,
}

/// One pull request in [`NeedsYou`].
#[derive(Debug, Clone, Serialize)]
pub struct PrWait {
    pub thread_id: i64,
    pub app_id: i64,
    /// `comments` or `merge`.
    pub why: &'static str,
}

/// A watched pull request, as read.
struct Watch {
    app_id: i64,
    pr: LastPr,
    /// The owner dismissed the work that opened it.
    dismissed: bool,
}

/// Open comments on one open pull request, by who wrote them.
#[derive(Default)]
struct Comments {
    number: i64,
    /// (author, count), most comments first.
    by: Vec<(String, i64)>,
    newest: String,
}

/// What a row's words are made from.
#[derive(Default)]
struct Facts {
    total_work: i64,
    running: i64,
    queued: i64,
    work_needs_you: i64,
    work_in_review: i64,
    /// Work that could not finish, not dismissed.
    work_failed: i64,
    /// Accepted work that did not reach GitHub, not dismissed.
    work_not_sent: i64,
    plans_ready: i64,
    goals_for_review: i64,
    goals_talking: i64,
    goals_stuck: i64,
    lead_thinking: bool,
    working_goal: bool,
    pieces: Option<Pieces>,
    reviewer: Option<String>,
    open_prs: i64,
    newest_open: Option<i64>,
    /// The app's rules let the owner merge its open pull requests here.
    can_merge: bool,
    comments: Option<Comments>,
    last_pr: Option<LastPr>,
    /// The app's newest pull request has been looked at, whether or not it
    /// became `last_pr`.
    newest_seen: bool,
    deploy: Option<crate::deploy::Card>,
}

fn plural(n: i64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "param", "param and kim", "param and 2 others".
fn who(by: &[(String, i64)]) -> String {
    match by {
        [] => "Someone".to_owned(),
        [a] => a.0.clone(),
        [a, b] => format!("{} and {}", a.0, b.0),
        [a, rest @ ..] => format!("{} and {} others", a.0, rest.len()),
    }
}

/// Picks the row's tone and words. The first thing that waits on the owner
/// wins the line; the others are counted after it.
fn words(f: &Facts) -> (Tone, &'static str, String) {
    let mut asks: Vec<String> = Vec::new();
    if let Some(c) = &f.comments {
        let n: i64 = c.by.iter().map(|(_, n)| n).sum();
        asks.push(format!(
            "PR #{} · {} left {}",
            c.number,
            who(&c.by),
            plural(n, "comment", "comments")
        ));
    }
    if f.plans_ready > 0 {
        asks.push("Approve the plan".to_owned());
    }
    if f.goals_for_review > 0 {
        asks.push("Review the finished work".to_owned());
    }
    if f.goals_talking > 0 {
        asks.push("Answer your project lead".to_owned());
    }
    if f.goals_stuck > 0 {
        asks.push("Your project lead got stuck. Try again".to_owned());
    }
    match f.deploy.as_ref().map(|d| d.state.as_str()) {
        Some("waiting") => asks.push("Merged · ready to deploy".to_owned()),
        Some("failed") => asks.push("Deploy failed. Try again".to_owned()),
        _ => {}
    }
    if f.work_needs_you > 0 {
        asks.push(format!(
            "{} you",
            plural(f.work_needs_you, "piece needs", "pieces need")
        ));
    }
    if f.work_in_review > 0 {
        asks.push(format!(
            "{} to check",
            plural(f.work_in_review, "piece is ready", "pieces are ready")
        ));
    }
    if f.work_failed > 0 {
        asks.push(format!(
            "{} finish",
            plural(f.work_failed, "piece couldn't", "pieces couldn't")
        ));
    }
    if f.work_not_sent > 0 {
        asks.push(format!(
            "{} GitHub",
            plural(f.work_not_sent, "piece didn't reach", "pieces didn't reach")
        ));
    }
    if let Some(first) = asks.first() {
        let line = match asks.len() {
            1 => first.clone(),
            n => format!("{first} · {} more", n - 1),
        };
        return (Tone::NeedsYou, "Needs you", line);
    }

    if let Some(d) = f.deploy.as_ref().filter(|d| d.state == "deploying") {
        return (Tone::Working, "Working", format!("Deploying · {}", d.line));
    }

    if f.working_goal || f.running > 0 || f.queued > 0 || f.lead_thinking {
        let line = match (&f.pieces, f.running, f.queued) {
            (Some(p), _, _) if p.total > 0 => format!("{} of {} pieces built", p.done, p.total),
            (_, 0, 0) if f.lead_thinking => "Your project lead is thinking".to_owned(),
            (_, 0, 0) => "Being checked".to_owned(),
            (_, r, 0) => format!("{} running", plural(r, "piece", "pieces")),
            (_, 0, q) => format!("{} waiting to start", plural(q, "piece", "pieces")),
            (_, r, q) => format!("{r} running, {q} waiting to start"),
        };
        return (Tone::Working, "Working", line);
    }

    if let (Some(number), true) = (f.newest_open, f.open_prs > 0) {
        let pr = if f.open_prs == 1 {
            format!("PR #{number}")
        } else {
            format!("{} open", plural(f.open_prs, "PR", "PRs"))
        };
        return match &f.reviewer {
            Some(r) => (
                Tone::Reviewer,
                "With reviewer",
                format!("{pr} · waiting on {r}"),
            ),
            None if f.can_merge => (Tone::Merge, "Your merge", format!("{pr} · yours to merge")),
            None => (
                Tone::Merge,
                "Your merge",
                format!("{pr} · yours to merge on GitHub"),
            ),
        };
    }

    if f.deploy.as_ref().is_some_and(|d| d.state == "live") {
        return (Tone::Done, "Live", "Live".to_owned());
    }

    match &f.last_pr {
        Some(p) if p.state == "merged" => (Tone::Done, "Done", format!("PR #{} merged", p.number)),
        Some(p) if p.state == "closed" => (
            Tone::Quiet,
            "Quiet",
            format!("PR #{} closed without merging", p.number),
        ),
        _ if f.total_work == 0 => (Tone::Quiet, "Quiet", "No work yet".to_owned()),
        _ => (Tone::Quiet, "Quiet", "Nothing waiting".to_owned()),
    }
}

impl Store {
    /// What only the owner can move on now ([`NeedsYou`]).
    pub async fn needs_you(&self) -> Result<NeedsYou> {
        let mut n = NeedsYou::default();
        // A piece of a goal in review with its check still to come waits on
        // the checker, not the owner (the same rule as its work panel).
        for r in sqlx::query(
            "SELECT t.id, t.app_id, t.state FROM threads t
             LEFT JOIN goal_pieces p ON p.thread_id = t.id
             WHERE t.dismissed_at IS NULL AND (
                 t.state IN ('needs_you', 'failed')
                 OR (t.state = 'done' AND t.gh_state = 'failed')
                 OR (t.state = 'in_review' AND NOT (p.thread_id IS NOT NULL AND t.research = 0
                     AND p.review IN ('building', 'reviewing', 'fixing'))))
             ORDER BY t.id",
        )
        .fetch_all(self.pool())
        .await?
        {
            n.work.push(WorkWait {
                thread_id: r.try_get("id")?,
                app_id: r.try_get("app_id")?,
                why: match r.try_get::<String, _>("state")?.as_str() {
                    "needs_you" => "answer",
                    "in_review" => "check",
                    "failed" => "failed",
                    _ => "not_sent",
                },
            });
        }
        for g in self.goals().await? {
            let why = match g.state {
                GoalState::Done | GoalState::Stopped => continue,
                _ if g.stuck && !g.busy => "stuck",
                GoalState::Talking | GoalState::PlanReady if g.busy => continue,
                GoalState::PlanReady => "plan",
                GoalState::Talking => "answer",
                GoalState::ForReview => "review",
                GoalState::Working => continue,
            };
            n.goals.push(GoalWait {
                goal_id: g.id,
                app_id: g.app_id,
                why,
            });
        }
        for w in self.watched_prs().await? {
            if w.state != "open" || w.fixing {
                continue;
            }
            let why = if w.open_comments > 0 {
                "comments"
            } else if w.can_merge {
                "merge"
            } else {
                continue;
            };
            n.prs.push(PrWait {
                thread_id: w.thread_id,
                app_id: w.app_id,
                why,
            });
        }
        let apps = n
            .work
            .iter()
            .map(|w| w.app_id)
            .chain(n.goals.iter().map(|g| g.app_id))
            .chain(n.prs.iter().map(|p| p.app_id));
        for app in apps {
            *n.by_app.entry(app).or_default() += 1;
            n.total += 1;
        }
        Ok(n)
    }

    /// The Today strip: every app, those needing the owner first, then those
    /// with work running, then the rest; by name within each, with today's
    /// starred apps ahead.
    pub async fn today(&self) -> Result<Vec<TodayRow>> {
        let apps = self.apps().await?;
        let goals = self.goals().await?;
        let mut facts: HashMap<i64, Facts> = HashMap::new();

        for r in
            sqlx::query("SELECT app_id, state, COUNT(*) AS n FROM threads GROUP BY app_id, state")
                .fetch_all(self.pool())
                .await?
        {
            let f = facts.entry(r.try_get("app_id")?).or_default();
            let n: i64 = r.try_get("n")?;
            f.total_work += n;
            match r.try_get::<String, _>("state")?.as_str() {
                "running" => f.running += n,
                "queued" => f.queued += n,
                _ => {}
            }
        }
        // Work that waits on the owner, as the one list counts it.
        let needs = self.needs_you().await?;
        for w in &needs.work {
            let f = facts.entry(w.app_id).or_default();
            match w.why {
                "answer" => f.work_needs_you += 1,
                "check" => f.work_in_review += 1,
                "failed" => f.work_failed += 1,
                _ => f.work_not_sent += 1,
            }
        }

        // Goals come newest first, so the first working goal seen for an app
        // is its newest: the one whose pieces the row counts.
        let mut newest_working: HashMap<i64, i64> = HashMap::new();
        for g in &goals {
            if matches!(g.state, GoalState::Done | GoalState::Stopped) {
                continue;
            }
            let f = facts.entry(g.app_id).or_default();
            let talking = matches!(g.state, GoalState::Talking | GoalState::PlanReady);
            if g.stuck && !g.busy {
                f.goals_stuck += 1;
            } else if talking && g.busy {
                f.lead_thinking = true;
            } else {
                match g.state {
                    GoalState::PlanReady => f.plans_ready += 1,
                    GoalState::Talking => f.goals_talking += 1,
                    GoalState::ForReview => f.goals_for_review += 1,
                    GoalState::Working => {
                        if !f.working_goal {
                            newest_working.insert(g.id, g.app_id);
                        }
                        f.working_goal = true;
                    }
                    _ => {}
                }
            }
        }
        for r in sqlx::query(
            "SELECT p.goal_id, COUNT(*) AS total,
                    SUM(CASE WHEN t.state = 'done' OR p.review IN ('passed', 'needs_a_look')
                        THEN 1 ELSE 0 END) AS done
             FROM goal_pieces p JOIN threads t ON t.id = p.thread_id
             JOIN goals g ON g.id = p.goal_id
             WHERE g.state = 'working' GROUP BY p.goal_id",
        )
        .fetch_all(self.pool())
        .await?
        {
            let goal_id: i64 = r.try_get("goal_id")?;
            if let Some(app_id) = newest_working.get(&goal_id) {
                facts.entry(*app_id).or_default().pieces = Some(Pieces {
                    goal_id,
                    done: r.try_get("done")?,
                    total: r.try_get("total")?,
                });
            }
        }

        // Newest happening first: when it last changed, else when it opened.
        let mut watches = Vec::new();
        for r in sqlx::query(
            "SELECT w.thread_id, w.app_id, w.number, w.url, w.state, w.event, w.event_at,
                    COALESCE(t.dismissed_at IS NOT NULL, 0) AS dismissed,
                    (w.state = 'open' AND a.can_merge) AS can_merge,
                    CASE WHEN w.state = 'open' AND w.health_head = w.head THEN w.health END AS health,
                    w.base_ref,
                    (w.state = 'open' AND NOT EXISTS (SELECT 1 FROM threads f WHERE f.fix_of = w.thread_id
                        AND (f.state NOT IN ('done', 'failed')
                             OR (f.state = 'done' AND f.gh_state IN ('waiting', 'sending'))))) AS can_update
             FROM pr_watches w LEFT JOIN threads t ON t.id = w.thread_id
                  JOIN apps a ON a.id = w.app_id
             ORDER BY COALESCE(w.event_at, w.created_at) DESC, w.created_at DESC, w.thread_id DESC",
        )
        .fetch_all(self.pool())
        .await?
        {
            watches.push(Watch {
                app_id: r.try_get("app_id")?,
                pr: LastPr {
                    thread_id: r.try_get("thread_id")?,
                    number: r.try_get("number")?,
                    url: r.try_get("url")?,
                    state: r.try_get("state")?,
                    event: r.try_get("event")?,
                    event_at: r.try_get("event_at")?,
                    can_merge: r.try_get("can_merge")?,
                    health: crate::pr_watch::health_words(
                        r.try_get::<Option<String>, _>("health")?.as_deref(),
                        r.try_get::<Option<String>, _>("base_ref")?.as_deref(),
                    ),
                    can_update: r.try_get("can_update")?,
                },
                dismissed: r.try_get("dismissed")?,
            });
        }
        for w in &watches {
            let f = facts.entry(w.app_id).or_default();
            if !f.newest_seen {
                f.newest_seen = true;
                // A merged or closed pull request whose work the owner
                // dismissed is old news: the row goes quiet instead of
                // repeating it, and an older one does not take its place.
                if !(w.dismissed && w.pr.state != "open") {
                    f.last_pr = Some(w.pr.clone());
                }
            }
            if w.pr.state == "open" {
                f.can_merge |= w.pr.can_merge;
                f.open_prs += 1;
                f.newest_open.get_or_insert(w.pr.number);
            }
        }

        // Comments waiting for a fix (as `pr_watch` counts them), on pull
        // requests still open.
        let mut by_pr: HashMap<i64, (i64, Comments)> = HashMap::new();
        let mut open_comments: HashMap<i64, i64> = HashMap::new();
        for r in sqlx::query(&format!(
            "SELECT c.thread_id, w.app_id, w.number, c.author, COUNT(*) AS n, MAX(c.at) AS newest
             FROM pr_comments c JOIN pr_watches w ON w.thread_id = c.thread_id
             WHERE w.state = 'open' AND {}
             GROUP BY c.thread_id, c.author ORDER BY n DESC, c.author",
            crate::pr_watch::OPEN_COMMENT
        ))
        .fetch_all(self.pool())
        .await?
        {
            let app_id: i64 = r.try_get("app_id")?;
            let n: i64 = r.try_get("n")?;
            let newest: String = r.try_get("newest")?;
            *open_comments.entry(app_id).or_default() += n;
            let (_, c) = by_pr
                .entry(r.try_get("thread_id")?)
                .or_insert_with(|| (app_id, Comments::default()));
            c.number = r.try_get("number")?;
            c.by.push((r.try_get("author")?, n));
            if newest > c.newest {
                c.newest = newest;
            }
        }
        // "Answer the comments" answers the pull request with the newest comment.
        let mut by_pr: Vec<(i64, (i64, Comments))> = by_pr.into_iter().collect();
        by_pr
            .sort_by(|(a_id, (_, a)), (b_id, (_, b))| b.newest.cmp(&a.newest).then(b_id.cmp(a_id)));
        let mut fix_thread: HashMap<i64, i64> = HashMap::new();
        for (thread_id, (app_id, c)) in by_pr {
            if let std::collections::hash_map::Entry::Vacant(e) = fix_thread.entry(app_id) {
                e.insert(thread_id);
                facts.entry(app_id).or_default().comments = Some(c);
            }
        }

        let can_deploy: std::collections::HashSet<i64> =
            sqlx::query_scalar::<_, i64>("SELECT id FROM apps WHERE can_deploy = 1")
                .fetch_all(self.pool())
                .await?
                .into_iter()
                .collect();
        for d in self.deploy_cards().await? {
            let app_id = d.app_id;
            facts.entry(app_id).or_default().deploy = Some(d);
        }

        let mut used = crate::tokens::today_by_app(self.pool()).await?;
        let mut rows: Vec<TodayRow> = apps
            .into_iter()
            .map(|a| {
                let mut f = facts.remove(&a.id).unwrap_or_default();
                f.reviewer = a.reviewer.clone();
                let (tone, status, line) = words(&f);
                TodayRow {
                    app_id: a.id,
                    name: a.name,
                    first_today: a.first_today,
                    running: f.running,
                    queued: f.queued,
                    needs_you: needs.by_app.get(&a.id).copied().unwrap_or(0),
                    pieces: f.pieces,
                    reviewer: a.reviewer,
                    waiting_on_reviewer: f.open_prs,
                    open_comments: open_comments.get(&a.id).copied().unwrap_or(0),
                    fix_thread_id: fix_thread.get(&a.id).copied(),
                    last_pr: f.last_pr,
                    deploy: f.deploy,
                    can_deploy: can_deploy.contains(&a.id),
                    tone,
                    status: status.to_owned(),
                    line,
                    used_today: used.remove(&a.id),
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            a.tone
                .rank()
                .cmp(&b.tone.rank())
                .then(b.first_today.cmp(&a.first_today))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then(a.app_id.cmp(&b.app_id))
        });
        Ok(rows)
    }
}

/// `GET /api/needs-you`: owner only.
pub(crate) async fn needs(State(st): State<AppState>, Extension(c): Extension<Caller>) -> Response {
    if c != Caller::Owner {
        return forbidden("seeing what needs you");
    }
    match st.store.needs_you().await {
        Ok(n) => Json(n).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// `GET /api/today`: owner only.
pub(crate) async fn list(State(st): State<AppState>, Extension(c): Extension<Caller>) -> Response {
    if c != Caller::Owner {
        return forbidden("seeing every app at a glance");
    }
    match st.store.today().await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_authors_are_named_in_plain_words() {
        let one = vec![("param".to_owned(), 3)];
        assert_eq!(who(&one), "param");
        let three = vec![
            ("param".to_owned(), 3),
            ("kim".to_owned(), 1),
            ("lee".to_owned(), 1),
        ];
        assert_eq!(who(&three), "param and 2 others");
    }

    #[test]
    fn the_first_thing_waiting_on_the_owner_wins_and_the_rest_are_counted() {
        let f = Facts {
            plans_ready: 1,
            work_needs_you: 2,
            running: 1,
            ..Facts::default()
        };
        let (tone, status, line) = words(&f);
        assert_eq!(tone, Tone::NeedsYou);
        assert_eq!(status, "Needs you");
        assert_eq!(line, "Approve the plan · 1 more");
    }

    #[test]
    fn an_open_pull_request_without_a_reviewer_is_the_owners_to_merge() {
        let f = Facts {
            open_prs: 1,
            newest_open: Some(7),
            ..Facts::default()
        };
        let (tone, status, line) = words(&f);
        assert_eq!(tone, Tone::Merge);
        assert_eq!(status, "Your merge");
        assert_eq!(line, "PR #7 · yours to merge on GitHub");
    }
}
