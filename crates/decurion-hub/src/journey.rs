//! One road for a whole goal, from the owner's ask to live: its journey.
//!
//! The goals list, a goal's page, the board, the phone and the roadmap all
//! show a goal's stages and its status words from here, so moving between
//! them never shows the same goal two ways. The stages are always Ask,
//! Plan, Build, Check, Merge, and Live (or "On your PC" for Decurion's own
//! code) only for an app with a step after the merge. Rules that must stay
//! true:
//!
//! - **One source of words.** Every word about a piece's way to GitHub and
//!   beyond comes from its road ([`crate::road`]); the words here only add
//!   what a goal has and a piece does not (the talk, the plan, the
//!   project lead), and are built from the app's rules where a rule decides
//!   them, never written as a fixed sentence that could be untrue.
//! - **You means the owner.** A stage is marked You, and `you` is set,
//!   only when the next move is the owner's (or their reviewer's on GitHub,
//!   as the road says). A goal piece that waits on the owner makes its goal
//!   wait: the one needs-you count ([`crate::today`]) counts it once, as
//!   its goal.
//! - **Only looks.** Building a journey moves nothing, sends nothing and
//!   asks no git beyond what [`Store::road_rules`] already keeps for a
//!   minute, so the goals list can ask for every goal's journey each time
//!   it refreshes.

use crate::road::{self, Rules};
use crate::store::{Result, Store};
use decurion_proto::goal::{
    Goal, GoalItem, GoalState, Journey, JourneyAction, JourneyMark, JourneyStep, ReviewState, Road,
    Stage, Step, StepMark,
};
use decurion_proto::ThreadState;
use sqlx::Row;
use std::collections::HashMap;

/// One piece of a goal, as its journey needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PieceNow {
    pub thread_id: i64,
    pub title: String,
    pub thread: ThreadState,
    pub review: ReviewState,
    /// Its road; `None` for a research piece, which changes no code.
    pub road: Option<Road>,
}

/// What the hub knows about a goal, for its journey.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalFacts {
    pub state: GoalState,
    /// The project lead is working on a reply.
    pub busy: bool,
    /// The project lead's last turn failed; the owner can try again.
    pub stuck: bool,
    /// Pieces in the goal's latest plan, and how many of them change code.
    pub planned: usize,
    pub planned_code: usize,
    /// The goal's pieces still on their way, in plan order ([`on_the_way`]).
    pub pieces: Vec<PieceNow>,
}

/// Whether a piece is still on its way: one that ended without reaching
/// GitHub (stopped or rejected) is off the road, the same rule the goal
/// page's road uses. `send_started` says it has a send (`gh_state`).
pub fn on_the_way(thread: ThreadState, send_started: bool) -> bool {
    thread != ThreadState::Failed || send_started
}

const STAGES: [Stage; 6] = [
    Stage::Ask,
    Stage::Plan,
    Stage::Build,
    Stage::Check,
    Stage::Merge,
    Stage::Live,
];

fn stage_label(s: Stage, r: &Rules) -> &'static str {
    match s {
        Stage::Ask => "Ask",
        Stage::Plan => "Plan",
        Stage::Build => "Build",
        Stage::Check => "Check",
        Stage::Merge => "Merge",
        // The same name whether it is done or not, as on a piece's road.
        Stage::Live if r.decurion => "On your PC",
        Stage::Live => "Live",
    }
}

/// The stages that apply to the app, each with the mark `mark` gives it.
fn steps(r: &Rules, mark: impl Fn(Stage) -> JourneyMark) -> Vec<JourneyStep> {
    STAGES
        .iter()
        .filter(|s| **s != Stage::Live || r.goes_live())
        .map(|&stage| JourneyStep {
            stage,
            label: stage_label(stage, r).to_owned(),
            mark: mark(stage),
        })
        .collect()
}

fn from_step(m: StepMark) -> JourneyMark {
    match m {
        StepMark::Done => JourneyMark::Done,
        StepMark::Now => JourneyMark::Now,
        StepMark::Todo => JourneyMark::Todo,
        StepMark::Skipped => JourneyMark::Skipped,
    }
}

/// Several steps of one road as one stage: on it while any is now, done
/// once all are done or passed by.
fn in_turn(marks: &[JourneyMark]) -> JourneyMark {
    use JourneyMark as M;
    if marks.is_empty() || marks.iter().all(|m| *m == M::Skipped) {
        M::Skipped
    } else if marks.iter().any(|m| matches!(m, M::Now | M::You)) {
        M::Now
    } else if marks.iter().all(|m| matches!(m, M::Done | M::Skipped)) {
        M::Done
    } else if marks.contains(&M::Done) {
        M::Now
    } else {
        M::Todo
    }
}

/// A research piece's report is written (or it was settled without one).
fn research_built(p: &PieceNow) -> bool {
    matches!(p.thread, ThreadState::InReview | ThreadState::Done) || p.review.is_settled()
}

fn research_checked(p: &PieceNow) -> bool {
    p.review == ReviewState::Passed || p.thread == ThreadState::Done
}

/// The road of the piece furthest behind, and that piece.
fn slowest(pieces: &[PieceNow]) -> Option<(&PieceNow, &Road)> {
    pieces
        .iter()
        .filter_map(|p| p.road.as_ref().map(|r| (p, r)))
        .min_by_key(|(_, r)| road::progress(r))
}

/// The same stage over every piece: not reached while no piece has
/// reached it, done once every piece is past it, and under way (Now) in
/// between, so a stage some pieces passed never looks untouched next to a
/// later stage another piece is on.
fn across(marks: &[JourneyMark]) -> Option<JourneyMark> {
    use JourneyMark as M;
    if marks.is_empty() {
        None
    } else if marks.iter().all(|m| *m == M::Skipped) {
        Some(M::Skipped)
    } else if marks.iter().all(|m| matches!(m, M::Done | M::Skipped)) {
        Some(M::Done)
    } else if marks.iter().all(|m| matches!(m, M::Todo | M::Skipped)) {
        Some(M::Todo)
    } else {
        Some(M::Now)
    }
}

/// Build, Check, Merge and Live, from where every piece is: each piece's
/// own road gives its mark for a stage, and [`across`] joins them, with
/// research pieces counted for Build and Check only.
fn work_marks(pieces: &[PieceNow]) -> impl Fn(Stage) -> JourneyMark {
    use JourneyMark as M;
    let of = |r: &Road, steps: &[Step]| -> JourneyMark {
        let marks: Vec<JourneyMark> = r
            .steps
            .iter()
            .filter(|s| steps.contains(&s.step))
            .map(|s| from_step(s.mark))
            .collect();
        in_turn(&marks)
    };
    let (mut build, mut check, mut merge, mut live) = (vec![], vec![], vec![], vec![]);
    for p in pieces {
        match &p.road {
            Some(r) => {
                build.push(of(r, &[Step::Built]));
                check.push(of(r, &[Step::Checked]));
                merge.push(of(r, &[Step::Pushed, Step::PullRequest, Step::Merged]));
                if r.steps.iter().any(|s| s.step == Step::Live) {
                    live.push(of(r, &[Step::Live]));
                }
            }
            None => {
                build.push(if research_built(p) { M::Done } else { M::Now });
                check.push(match (research_built(p), research_checked(p)) {
                    (false, _) => M::Todo,
                    (true, true) => M::Done,
                    (true, false) => M::Now,
                });
            }
        }
    }
    let build = across(&build).unwrap_or(M::Skipped);
    let check = across(&check).unwrap_or(M::Skipped);
    let merge = across(&merge).unwrap_or(M::Skipped);
    let live = across(&live).unwrap_or(M::Skipped);
    move |s| match s {
        Stage::Ask | Stage::Plan => M::Done,
        Stage::Build => build,
        Stage::Check => check,
        Stage::Merge => merge,
        Stage::Live => live,
    }
}

/// What the owner is waited on for, and where.
struct Wait {
    /// The stage it marks You, if any.
    stage: Option<Stage>,
    label: String,
    next: String,
    kind: &'static str,
    button: String,
    piece: Option<i64>,
}

impl Wait {
    fn lead(
        stage: Option<Stage>,
        label: &str,
        next: String,
        kind: &'static str,
        button: &str,
    ) -> Self {
        Self {
            stage,
            label: label.to_owned(),
            next,
            kind,
            button: button.to_owned(),
            piece: None,
        }
    }
}

const STUCK: &str = "Your project lead ran into a problem. Nothing was lost. Tap Try again.";
const AGENT_ASKS: &str = "The agent stopped to ask you something. Answer it, and it carries on.";
const REVIEW: &str =
    "Everything is built and checked. Open the goal to see the report and what to do next.";

/// The first piece that waits on the owner: one whose agent asked
/// something first, then the first in plan order whose road waits on them.
fn piece_wait(pieces: &[PieceNow], r: &Rules) -> Option<Wait> {
    let many = pieces.len() > 1;
    let says = |p: &PieceNow, next: &str| {
        if many {
            format!("“{}”: {next}", p.title)
        } else {
            next.to_owned()
        }
    };
    if let Some(p) = pieces.iter().find(|p| p.thread == ThreadState::NeedsYou) {
        return Some(Wait {
            stage: Some(Stage::Build),
            label: "Needs you · answer the agent".into(),
            next: says(p, AGENT_ASKS),
            kind: "answer",
            button: "Answer the agent".into(),
            piece: Some(p.thread_id),
        });
    }
    for p in pieces {
        match &p.road {
            Some(road) if road.you && !road.done => {
                let at = road
                    .steps
                    .iter()
                    .find(|s| s.mark == StepMark::Now)
                    .map(|s| s.step);
                let (stage, kind, button) = match at {
                    Some(Step::Checked) => (Stage::Check, "review", "Take a look"),
                    Some(Step::Pushed) if road.label == "Needs you · not pushed" => {
                        (Stage::Merge, "retry", "Try again")
                    }
                    Some(Step::Pushed) => (Stage::Merge, "review", "Look it over"),
                    Some(Step::PullRequest) => (Stage::Merge, "open_pr", "Open the pull request"),
                    Some(Step::Merged) if r.merge => (Stage::Merge, "merge", "Merge"),
                    Some(Step::Merged) => (Stage::Merge, "merge", "Merge on GitHub"),
                    Some(Step::Live) if r.decurion => (Stage::Live, "update", "Update your PC"),
                    Some(Step::Live) => (Stage::Live, "deploy", "Deploy"),
                    // Built is the only step left: work that could not be
                    // finished but was sent, which the road says to retry.
                    Some(Step::Built) | None => (Stage::Build, "retry", "Try again"),
                };
                return Some(Wait {
                    stage: Some(stage),
                    label: road.label.clone(),
                    next: says(p, &road.next),
                    kind,
                    button: button.into(),
                    piece: Some(p.thread_id),
                });
            }
            // A research piece in review once its check is over is the
            // owner's to read: it has no Accept and nothing goes to GitHub.
            None if p.thread == ThreadState::InReview && p.review.is_settled() => {
                let (label, next, button) = if p.review == ReviewState::Passed {
                    (
                        "Needs you · read the report",
                        "Its report is ready. Read it, then tap Fix what it found or Mark done.",
                        "Read the report",
                    )
                } else {
                    (
                        "Needs you · take a look",
                        "The checker could not pass it. Look it over, then tap Fix what it found or Mark done.",
                        "Take a look",
                    )
                };
                return Some(Wait {
                    stage: Some(Stage::Check),
                    label: label.into(),
                    next: says(p, next),
                    kind: "review",
                    button: button.into(),
                    piece: Some(p.thread_id),
                });
            }
            _ => {}
        }
    }
    None
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "a", "a and b", "a, b and c".
fn and_list(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What is running while nothing waits on the owner, in one plain line.
fn underway(pieces: &[PieceNow]) -> String {
    let (mut build, mut check, mut send) = (0, 0, 0);
    for p in pieces {
        match &p.road {
            Some(road) if !road.done && !road.you => {
                match road
                    .steps
                    .iter()
                    .find(|s| s.mark == StepMark::Now)
                    .map(|s| s.step)
                {
                    Some(Step::Built) => build += 1,
                    Some(Step::Checked) => check += 1,
                    Some(Step::Pushed) => send += 1,
                    _ => {}
                }
            }
            Some(_) => {}
            None if !research_built(p) => build += 1,
            None if !research_checked(p) && !p.review.is_settled() => check += 1,
            None => {}
        }
    }
    let mut parts = Vec::new();
    for (n, one, many) in [
        (build, "is being built", "are being built"),
        (check, "is being checked", "are being checked"),
        (
            send,
            "is on its way to GitHub",
            "are on their way to GitHub",
        ),
    ] {
        if n == 0 {
            continue;
        }
        let verb = if n == 1 { one } else { many };
        parts.push(if parts.is_empty() {
            format!("{} {verb}", plural(n, "piece", "pieces"))
        } else {
            format!("{n} {verb}")
        });
    }
    if parts.is_empty() {
        "Every piece is through, and your project lead sums it up next. Nothing needs you right now.".to_owned()
    } else {
        format!("{}. Nothing needs you right now.", and_list(&parts))
    }
}

/// What approving the plan starts, built from the app's rules.
fn approve_next(planned: usize, code: usize, r: &Rules) -> String {
    let start = match planned {
        0 => "Approve the plan to start the work.".to_owned(),
        1 => "Approve the plan to start its one piece.".to_owned(),
        n => format!("Approve the plan to start its {n} pieces."),
    };
    if code == 0 {
        let what = if planned == 1 {
            "It looks into things and writes a report."
        } else {
            "They look into things and write reports."
        };
        return format!("{start} {what} Nothing changes your app's code.");
    }
    let one = code == 1;
    let who = match (code == planned, one) {
        (true, true) => "It is",
        (true, false) => "Each is",
        (false, true) => "The one that changes code is",
        (false, false) => "The ones that change code are each",
    };
    let then = match (r.send_checked, one) {
        (true, true) => "and if it passes it goes to GitHub by itself.",
        (true, false) => "and each that passes goes to GitHub by itself.",
        (false, true) => {
            "and if it passes it waits for you to tap Accept, as this app's rules say."
        }
        (false, false) => {
            "and each that passes waits for you to tap Accept, as this app's rules say."
        }
    };
    format!("{start} {who} built, then checked by a second agent, {then}")
}

fn finish(
    steps: Vec<JourneyStep>,
    label: String,
    next: String,
    you: bool,
    done: bool,
    action: Option<(&str, String)>,
    piece: Option<i64>,
) -> Journey {
    Journey {
        steps,
        label,
        you,
        done,
        next,
        action: action.map(|(kind, label)| JourneyAction {
            kind: kind.to_owned(),
            label,
        }),
        piece,
    }
}

/// Marks the stage a wait is about You.
fn mark_wait(steps: &mut [JourneyStep], wait: Option<&Wait>) {
    if let Some(stage) = wait.and_then(|w| w.stage) {
        for s in steps.iter_mut().filter(|s| s.stage == stage) {
            s.mark = JourneyMark::You;
        }
    }
}

fn from_wait(mut steps: Vec<JourneyStep>, w: Wait) -> Journey {
    mark_wait(&mut steps, Some(&w));
    finish(
        steps,
        w.label,
        w.next,
        true,
        false,
        Some((w.kind, w.button)),
        w.piece,
    )
}

/// A goal's journey: pure, so every case is tested without a database.
pub fn journey(f: &GoalFacts, r: &Rules) -> Journey {
    match f.state {
        GoalState::Talking | GoalState::PlanReady => talking(f, r),
        GoalState::Working | GoalState::ForReview => working(f, r),
        GoalState::Done => closed(f, r),
        GoalState::Stopped => stopped(f, r),
    }
}

/// Talking it through, or a plan waiting: the project lead's turn or the
/// owner's. A piece from an earlier round that waits on the owner still
/// counts while the project lead is not waiting on them.
fn talking(f: &GoalFacts, r: &Rules) -> Journey {
    let at = if f.state == GoalState::Talking {
        Stage::Ask
    } else {
        Stage::Plan
    };
    let steps = steps(r, |s| match s {
        Stage::Ask if at == Stage::Plan => JourneyMark::Done,
        s if s == at => JourneyMark::Now,
        _ => JourneyMark::Todo,
    });
    let lead = if f.busy {
        None
    } else if f.stuck {
        Some(Wait::lead(
            Some(at),
            "Needs you · try again",
            STUCK.into(),
            "retry",
            "Try again",
        ))
    } else if at == Stage::Ask {
        let mut next = "Your project lead is waiting for your reply.".to_owned();
        if f.pieces.is_empty() {
            next.push_str(" Nothing is built until you approve a plan.");
        }
        Some(Wait::lead(
            Some(at),
            "Needs you · reply",
            next,
            "reply",
            "Reply",
        ))
    } else {
        Some(Wait::lead(
            Some(at),
            "Needs you · approve the plan",
            approve_next(f.planned, f.planned_code, r),
            "approve",
            "Approve the plan",
        ))
    };
    match lead.or_else(|| piece_wait(&f.pieces, r)) {
        Some(w) => from_wait(steps, w),
        None => {
            let next = if at == Stage::Ask {
                "Your project lead is writing back. Nothing needs you right now."
            } else {
                "Your project lead is writing back. Then the plan waits for your approval."
            };
            finish(
                steps,
                "Talking it through".into(),
                next.into(),
                false,
                false,
                None,
                None,
            )
        }
    }
}

/// Pieces being built, checked and sent; or all through and the report
/// waiting for the owner.
fn working(f: &GoalFacts, r: &Rules) -> Journey {
    let steps = steps(r, work_marks(&f.pieces));
    let wait = piece_wait(&f.pieces, r)
        .or_else(|| {
            (f.stuck && !f.busy).then(|| {
                Wait::lead(
                    None,
                    "Needs you · try again",
                    STUCK.into(),
                    "retry",
                    "Try again",
                )
            })
        })
        .or_else(|| {
            (f.state == GoalState::ForReview).then(|| {
                // Looking at the work is the owner's next step wherever the
                // goal is on its way.
                let stage = steps
                    .iter()
                    .find(|s| s.mark == JourneyMark::Now)
                    .map(|s| s.stage);
                Wait::lead(
                    stage,
                    "Needs you · review the work",
                    REVIEW.into(),
                    "review",
                    "Review the work",
                )
            })
        });
    match wait {
        Some(w) => from_wait(steps, w),
        None => finish(
            steps,
            "Working".into(),
            underway(&f.pieces),
            false,
            false,
            None,
            None,
        ),
    }
}

/// Closed as done: every stage done, or the step after the merge still to
/// come, or closed before its work was merged.
fn closed(f: &GoalFacts, r: &Rules) -> Journey {
    let planned = f.planned > 0 || !f.pieces.is_empty();
    let Some((piece, slow)) = slowest(&f.pieces) else {
        let built = !f.pieces.is_empty();
        let steps = steps(r, |s| match s {
            Stage::Ask => JourneyMark::Done,
            Stage::Plan if planned => JourneyMark::Done,
            Stage::Build | Stage::Check if built => JourneyMark::Done,
            _ => JourneyMark::Skipped,
        });
        return finish(
            steps,
            "Done".into(),
            "This goal is done. Nothing more to do.".into(),
            false,
            true,
            None,
            None,
        );
    };
    let mut steps = steps(r, work_marks(&f.pieces));
    let at = slow
        .steps
        .iter()
        .find(|s| s.mark == StepMark::Now)
        .map(|s| s.step);
    if slow.done {
        let label = if r.decurion {
            "On your PC"
        } else if r.deploy {
            "Live"
        } else {
            "Done"
        };
        return finish(
            steps,
            label.into(),
            slow.next.clone(),
            false,
            true,
            None,
            None,
        );
    }
    if at == Some(Step::Live) {
        let (label, kind, button) = if r.decurion {
            ("Merged · not on your PC yet", "update", "Update your PC")
        } else {
            ("Merged · not live yet", "deploy", "Deploy")
        };
        return finish(
            steps,
            label.into(),
            slow.next.clone(),
            false,
            true,
            Some((kind, button.into())),
            Some(piece.thread_id),
        );
    }
    for s in &mut steps {
        if matches!(s.mark, JourneyMark::Now | JourneyMark::You) {
            s.mark = JourneyMark::Todo;
        }
    }
    finish(
        steps,
        "Closed".into(),
        "You closed this goal before all its work was merged.".into(),
        false,
        true,
        None,
        None,
    )
}

/// Stopped by the owner: what was done stays done, the rest was passed by.
fn stopped(f: &GoalFacts, r: &Rules) -> Journey {
    let planned = f.planned > 0 || !f.pieces.is_empty();
    let mut steps = if f.pieces.is_empty() {
        steps(r, |s| match s {
            Stage::Ask if planned => JourneyMark::Done,
            _ => JourneyMark::Skipped,
        })
    } else {
        steps(r, work_marks(&f.pieces))
    };
    for s in &mut steps {
        if s.mark != JourneyMark::Done {
            s.mark = JourneyMark::Skipped;
        }
    }
    finish(
        steps,
        "Stopped".into(),
        "This goal was stopped.".into(),
        false,
        false,
        None,
        None,
    )
}

impl Store {
    /// The journeys of `goals`, by goal id: one read of their pieces, one
    /// of their latest plans' sizes (only for goals that need it), and the
    /// rules asked once per app.
    pub async fn journeys(&self, goals: &[Goal]) -> Result<HashMap<i64, Journey>> {
        if goals.is_empty() {
            return Ok(HashMap::new());
        }
        let only = match goals {
            [one] => Some(one.id),
            _ => None,
        };
        let mut rules: HashMap<i64, Rules> = HashMap::new();
        for g in goals {
            if let std::collections::hash_map::Entry::Vacant(e) = rules.entry(g.app_id) {
                e.insert(self.road_rules(g.app_id).await?);
            }
        }
        let app_of: HashMap<i64, i64> = goals.iter().map(|g| (g.id, g.app_id)).collect();
        // The same pieces and pull request rows as the goal page reads.
        let rows = sqlx::query(&format!(
            "SELECT p.goal_id, p.thread_id, p.title, t.research, {} FROM goal_pieces p
             JOIN threads t ON t.id = p.thread_id
             LEFT JOIN pr_watches w ON w.thread_id = p.thread_id
             WHERE (?1 IS NULL OR p.goal_id = ?1) ORDER BY p.thread_id",
            road::FACT_COLS
        ))
        .bind(only)
        .fetch_all(self.pool())
        .await?;
        let mut pieces: HashMap<i64, Vec<PieceNow>> = HashMap::new();
        for row in &rows {
            let goal: i64 = row.try_get("goal_id")?;
            let Some(r) = app_of.get(&goal).and_then(|a| rules.get(a)) else {
                continue;
            };
            let facts = road::facts_from(row)?;
            if !on_the_way(facts.thread, facts.send.is_some()) {
                continue;
            }
            let research: bool = row.try_get("research")?;
            pieces.entry(goal).or_default().push(PieceNow {
                thread_id: row.try_get("thread_id")?,
                title: row.try_get("title")?,
                thread: facts.thread,
                review: facts.review,
                road: (!research).then(|| road::piece_road(&facts, r)),
            });
        }
        // The size of the latest plan, for goals still talking or waiting
        // on a plan, and for closed goals that never had pieces.
        let mut plans: HashMap<i64, (usize, usize)> = HashMap::new();
        for row in sqlx::query(
            "SELECT m.goal_id, json_array_length(m.body, '$.plan.pieces') AS planned,
                    (SELECT COUNT(*) FROM json_each(m.body, '$.plan.pieces') e
                     WHERE COALESCE(json_extract(e.value, '$.research'), 0) = 0) AS code
             FROM goal_messages m JOIN (
                 SELECT x.goal_id, MAX(x.id) AS id FROM goal_messages x JOIN goals g ON g.id = x.goal_id
                 WHERE (?1 IS NULL OR x.goal_id = ?1)
                   AND (g.state IN ('talking', 'plan_ready')
                        OR NOT EXISTS (SELECT 1 FROM goal_pieces gp WHERE gp.goal_id = g.id))
                   AND json_extract(x.body, '$.kind') = 'coordinator'
                   AND json_extract(x.body, '$.plan') IS NOT NULL
                 GROUP BY x.goal_id) l ON l.id = m.id",
        )
        .bind(only)
        .fetch_all(self.pool())
        .await?
        {
            let n = |col: &str| -> Result<usize> {
                Ok(usize::try_from(row.try_get::<Option<i64>, _>(col)?.unwrap_or(0)).unwrap_or(0))
            };
            plans.insert(row.try_get("goal_id")?, (n("planned")?, n("code")?));
        }
        let mut out = HashMap::new();
        for g in goals {
            let Some(r) = rules.get(&g.app_id) else {
                continue;
            };
            let (planned, planned_code) = plans.get(&g.id).copied().unwrap_or((0, 0));
            let facts = GoalFacts {
                state: g.state,
                busy: g.busy,
                stuck: g.stuck,
                planned,
                planned_code,
                pieces: pieces.remove(&g.id).unwrap_or_default(),
            };
            out.insert(g.id, journey(&facts, r));
        }
        Ok(out)
    }

    /// The goals list: every goal, newest first, each with its journey.
    pub async fn goal_items(&self) -> Result<Vec<GoalItem>> {
        let goals = self.goals().await?;
        let mut journeys = self.journeys(&goals).await?;
        Ok(goals
            .into_iter()
            .map(|g| GoalItem {
                journey: journeys.remove(&g.id).unwrap_or_default(),
                goal: g,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::road::{piece_road, Facts};
    use decurion_proto::SendState;

    fn facts(state: GoalState) -> GoalFacts {
        GoalFacts {
            state,
            busy: false,
            stuck: false,
            planned: 3,
            planned_code: 3,
            pieces: Vec::new(),
        }
    }

    fn piece_facts(thread: ThreadState, review: ReviewState) -> Facts {
        Facts {
            thread,
            checker: true,
            review,
            held: None,
            send: None,
            pull_request: false,
            on_github: false,
            branch: None,
            merged: false,
            closed: false,
            live: false,
            error: None,
        }
    }

    fn sent(pr: bool) -> Facts {
        Facts {
            send: Some(SendState::Sent),
            pull_request: pr,
            on_github: true,
            branch: Some("decurion/x".into()),
            ..piece_facts(ThreadState::Done, ReviewState::Passed)
        }
    }

    fn piece(id: i64, title: &str, f: &Facts, r: &Rules) -> PieceNow {
        PieceNow {
            thread_id: id,
            title: title.into(),
            thread: f.thread,
            review: f.review,
            road: Some(piece_road(f, r)),
        }
    }

    fn marks(j: &Journey) -> Vec<(&str, JourneyMark)> {
        j.steps.iter().map(|s| (s.label.as_str(), s.mark)).collect()
    }

    fn kind(j: &Journey) -> Option<&str> {
        j.action.as_ref().map(|a| a.kind.as_str())
    }

    use JourneyMark::{Done, Now, Skipped, Todo, You};

    #[test]
    fn talking_is_the_leads_turn_or_the_owners() {
        let r = Rules::default();
        let busy = journey(
            &GoalFacts {
                busy: true,
                ..facts(GoalState::Talking)
            },
            &r,
        );
        assert_eq!(busy.label, "Talking it through");
        assert!(!busy.you && busy.action.is_none());
        assert_eq!(
            marks(&busy),
            [
                ("Ask", Now),
                ("Plan", Todo),
                ("Build", Todo),
                ("Check", Todo),
                ("Merge", Todo)
            ]
        );

        let idle = journey(&facts(GoalState::Talking), &r);
        assert_eq!(idle.label, "Needs you · reply");
        assert!(idle.you);
        assert_eq!(kind(&idle), Some("reply"));
        assert_eq!(idle.steps[0].mark, You);
        assert!(idle
            .next
            .contains("Nothing is built until you approve a plan"));

        let stuck = journey(
            &GoalFacts {
                stuck: true,
                ..facts(GoalState::Talking)
            },
            &r,
        );
        assert_eq!(stuck.label, "Needs you · try again");
        assert_eq!(kind(&stuck), Some("retry"));
        assert!(stuck.you && stuck.piece.is_none());
    }

    #[test]
    fn a_ready_plan_says_what_approving_it_starts_by_the_rules() {
        let r = Rules::default();
        let j = journey(&facts(GoalState::PlanReady), &r);
        assert_eq!(j.label, "Needs you · approve the plan");
        assert_eq!(kind(&j), Some("approve"));
        assert!(j.you && !j.done);
        assert_eq!(
            marks(&j),
            [
                ("Ask", Done),
                ("Plan", You),
                ("Build", Todo),
                ("Check", Todo),
                ("Merge", Todo)
            ]
        );
        assert_eq!(
            j.next,
            "Approve the plan to start its 3 pieces. Each is built, then checked by a second agent, and each that passes goes to GitHub by itself."
        );
        let off = journey(
            &facts(GoalState::PlanReady),
            &Rules {
                send_checked: false,
                ..r
            },
        );
        assert!(!off.next.contains("by itself"), "{}", off.next);
        assert!(off.next.contains("tap Accept"), "{}", off.next);
        let one = journey(
            &GoalFacts {
                planned: 1,
                planned_code: 1,
                ..facts(GoalState::PlanReady)
            },
            &r,
        );
        assert!(one
            .next
            .starts_with("Approve the plan to start its one piece. It is built"));
        let research = journey(
            &GoalFacts {
                planned: 1,
                planned_code: 0,
                ..facts(GoalState::PlanReady)
            },
            &r,
        );
        assert!(research.next.contains("Nothing changes your app's code"));
    }

    #[test]
    fn an_agent_question_waits_on_the_owner_at_build() {
        let r = Rules::default();
        let asks = piece_facts(ThreadState::NeedsYou, ReviewState::Building);
        let builds = piece_facts(ThreadState::Running, ReviewState::Building);
        let j = journey(
            &GoalFacts {
                pieces: vec![piece(7, "Logo", &builds, &r), piece(8, "Help", &asks, &r)],
                ..facts(GoalState::Working)
            },
            &r,
        );
        assert_eq!(j.label, "Needs you · answer the agent");
        assert_eq!(kind(&j), Some("answer"));
        assert_eq!(j.piece, Some(8));
        assert!(
            j.next.starts_with("“Help”: The agent stopped"),
            "{}",
            j.next
        );
        assert_eq!(
            marks(&j),
            [
                ("Ask", Done),
                ("Plan", Done),
                ("Build", You),
                ("Check", Todo),
                ("Merge", Todo)
            ]
        );
    }

    #[test]
    fn a_merge_waiting_takes_the_label_from_the_road() {
        let r = Rules::default();
        let building = piece_facts(ThreadState::Running, ReviewState::Building);
        let j = journey(
            &GoalFacts {
                pieces: vec![
                    piece(3, "Logo", &sent(true), &r),
                    piece(4, "Help", &building, &r),
                ],
                ..facts(GoalState::Working)
            },
            &r,
        );
        assert_eq!(j.label, "Needs you · merge on GitHub");
        assert_eq!(kind(&j), Some("merge"));
        assert_eq!(
            j.action.as_ref().map(|a| a.label.as_str()),
            Some("Merge on GitHub")
        );
        assert_eq!(j.piece, Some(3));
        // One piece is past Check and one is still building, so Build and
        // Check are both under way, never an empty Check before a Merge
        // that waits on you.
        assert_eq!(j.steps[2].mark, Now);
        assert_eq!(j.steps[3].mark, Now);
        assert_eq!(j.steps[4].mark, You);

        let no_pr = journey(
            &GoalFacts {
                pieces: vec![piece(3, "Logo", &sent(false), &r)],
                ..facts(GoalState::Working)
            },
            &r,
        );
        assert_eq!(no_pr.label, "Needs you · open the pull request");
        assert_eq!(kind(&no_pr), Some("open_pr"));
        assert_eq!(no_pr.steps[4].mark, You);
    }

    #[test]
    fn working_with_nothing_waiting_says_what_runs() {
        let r = Rules::default();
        let b = piece_facts(ThreadState::Running, ReviewState::Building);
        let c = piece_facts(ThreadState::InReview, ReviewState::Reviewing);
        let j = journey(
            &GoalFacts {
                pieces: vec![
                    piece(1, "A", &b, &r),
                    piece(2, "B", &b, &r),
                    piece(3, "C", &c, &r),
                ],
                ..facts(GoalState::Working)
            },
            &r,
        );
        assert_eq!(j.label, "Working");
        assert!(!j.you && j.action.is_none());
        assert_eq!(
            j.next,
            "2 pieces are being built and 1 is being checked. Nothing needs you right now."
        );
        let looks = piece_facts(ThreadState::InReview, ReviewState::NeedsALook);
        let j = journey(
            &GoalFacts {
                pieces: vec![piece(5, "A", &looks, &r)],
                ..facts(GoalState::Working)
            },
            &r,
        );
        assert_eq!(j.label, "Needs you · take a look");
        assert_eq!(j.steps[3].mark, You);
        assert_eq!(kind(&j), Some("review"));
    }

    #[test]
    fn for_review_with_nothing_else_asks_to_review_the_work() {
        let r = Rules::default();
        let research = PieceNow {
            thread_id: 9,
            title: "Audit".into(),
            thread: ThreadState::Done,
            review: ReviewState::Passed,
            road: None,
        };
        let j = journey(
            &GoalFacts {
                pieces: vec![research],
                ..facts(GoalState::ForReview)
            },
            &r,
        );
        assert_eq!(j.label, "Needs you · review the work");
        assert_eq!(kind(&j), Some("review"));
        assert!(j.you && j.piece.is_none());
        assert_eq!(
            marks(&j),
            [
                ("Ask", Done),
                ("Plan", Done),
                ("Build", Done),
                ("Check", Done),
                ("Merge", Skipped)
            ]
        );
    }

    #[test]
    fn done_is_live_only_once_it_is() {
        let deploys = Rules {
            deploy: true,
            ..Rules::default()
        };
        let merged = Facts {
            merged: true,
            ..sent(true)
        };
        let j = journey(
            &GoalFacts {
                pieces: vec![piece(1, "A", &merged, &deploys)],
                ..facts(GoalState::Done)
            },
            &deploys,
        );
        assert_eq!(j.label, "Merged · not live yet");
        assert!(j.done && !j.you);
        assert_eq!(kind(&j), Some("deploy"));
        assert_eq!(
            j.steps.last().map(|s| (s.label.as_str(), s.mark)),
            Some(("Live", Now))
        );

        let live = Facts {
            live: true,
            ..merged.clone()
        };
        let j = journey(
            &GoalFacts {
                pieces: vec![piece(1, "A", &live, &deploys)],
                ..facts(GoalState::Done)
            },
            &deploys,
        );
        assert_eq!(j.label, "Live");
        assert!(j.done && j.steps.iter().all(|s| s.mark == Done));
    }

    #[test]
    fn an_app_without_live_ends_with_merge() {
        let r = Rules::default();
        let merged = Facts {
            merged: true,
            ..sent(true)
        };
        let j = journey(
            &GoalFacts {
                pieces: vec![piece(1, "A", &merged, &r)],
                ..facts(GoalState::Done)
            },
            &r,
        );
        assert_eq!(j.label, "Done");
        assert_eq!(j.steps.len(), 5);
        assert_eq!(j.steps.last().map(|s| s.label.as_str()), Some("Merge"));
        assert!(j.steps.iter().all(|s| s.mark == Done));
    }

    #[test]
    fn decurions_own_code_ends_on_your_pc() {
        let me = Rules {
            decurion: true,
            ..Rules::default()
        };
        let merged = Facts {
            merged: true,
            ..sent(true)
        };
        // Still working: updating the PC is the owner's move.
        let j = journey(
            &GoalFacts {
                pieces: vec![piece(1, "A", &merged, &me)],
                ..facts(GoalState::Working)
            },
            &me,
        );
        assert_eq!(j.label, "Merged · not on your PC yet");
        assert!(j.you);
        assert_eq!(kind(&j), Some("update"));
        assert_eq!(
            j.steps.last().map(|s| (s.label.as_str(), s.mark)),
            Some(("On your PC", You))
        );
        let on = Facts {
            live: true,
            ..merged
        };
        let j = journey(
            &GoalFacts {
                pieces: vec![piece(1, "A", &on, &me)],
                ..facts(GoalState::Done)
            },
            &me,
        );
        assert_eq!(j.label, "On your PC");
        assert!(j.done && !j.you);
    }

    #[test]
    fn a_stopped_goal_waits_on_nobody() {
        let r = Rules::default();
        let b = piece_facts(ThreadState::Running, ReviewState::Building);
        let j = journey(
            &GoalFacts {
                pieces: vec![piece(1, "A", &sent(true), &r), piece(2, "B", &b, &r)],
                ..facts(GoalState::Stopped)
            },
            &r,
        );
        assert_eq!(j.label, "Stopped");
        assert!(!j.you && !j.done && j.action.is_none());
        assert!(j
            .steps
            .iter()
            .all(|s| !matches!(s.mark, JourneyMark::Now | JourneyMark::You)));
        assert_eq!(j.steps[0].mark, Done);
        let talked = journey(
            &GoalFacts {
                planned: 0,
                ..facts(GoalState::Stopped)
            },
            &r,
        );
        assert!(talked.steps.iter().all(|s| s.mark == Skipped));
    }

    #[test]
    fn a_closed_goal_with_work_still_open_says_closed() {
        let r = Rules::default();
        let j = journey(
            &GoalFacts {
                pieces: vec![piece(1, "A", &sent(true), &r)],
                ..facts(GoalState::Done)
            },
            &r,
        );
        assert_eq!(j.label, "Closed");
        assert!(j.done && !j.you);
        assert!(j.steps.iter().all(|s| s.mark != Now && s.mark != You));
    }

    #[test]
    fn a_journey_in_json_names_its_stages_in_snake_case() {
        let j = journey(&facts(GoalState::PlanReady), &Rules::default());
        let v = serde_json::to_value(&j).expect("json");
        assert_eq!(v["steps"][1]["stage"], "plan");
        assert_eq!(v["steps"][1]["mark"], "you");
        assert_eq!(v["action"]["kind"], "approve");
        assert!(v.get("piece").is_none(), "{v}");
    }
}
