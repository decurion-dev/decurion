//! One story from request to live.
//!
//! Every screen that says where work stands (the goal page, a piece's work
//! panel, the plan and the box that approves it, and the card of any
//! finished work that went to GitHub) reads it from here, so they all tell
//! the same story, and the app's real rules decide it: whether checked work
//! goes to GitHub by itself, whether the owner merges from Decurion, whether
//! Decurion deploys the app, and whether the app is Decurion's own code.
//! The steps are always named the same way: Built, Checked, Pushed, Pull
//! request, Merged, and Live (or "On your PC" for Decurion's own code).
//! Work that is no piece of a goal has no checker, so no Checked step.
//! Rules that must stay true:
//!
//! - **Words follow the rules.** A line that depends on a rule is built
//!   from that rule here, never written as a fixed sentence on a page.
//! - **Only looks.** Nothing here moves work, sends, merges, deploys or
//!   writes to an app's folder. The two things it asks of an app's clone
//!   use read-only git (`cat-file -e`, `merge-base --is-ancestor`): never a
//!   fetch, pull, checkout or anything that changes it.
//! - **Pushed is not a pull request.** Work that went up as a branch with
//!   no pull request waits on the owner to open one, with the hub's reason
//!   why it could not; it never looks like work waiting to be merged.
//! - **The running hub never changes itself.** For Decurion's own code the
//!   step after Merged is "On your PC", and updating is the owner's to do.
//!   The hub only notices, after the owner updated and restarted it, that
//!   the commit it was built from ([`crate::HUB_COMMIT`]) holds the merge,
//!   and then marks it done. Nothing here updates or restarts Decurion.

use crate::checkpoints::git;
use crate::push::Notice;
use crate::store::{Result, Store, StoreError};
use decurion_proto::goal::{GoalState, ReviewState, Road, RoadStep, Step, StepMark};
use decurion_proto::{SendState, ThreadState};
use sqlx::Row;
use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A file only Decurion's own code has, read from the clone's latest
/// commit, so an app is known to be Decurion itself without asking GitHub.
const DECURION_MARK: &str = "crates/decurion-hub/build.rs";

/// How long what an app's clone said is kept before it is asked again.
const LOOK_SECS: u64 = 60;

/// The help page that says how to update Decurion.
pub const UPDATE_HELP: &str = "improve-decurion-itself";

/// The rules of one app that decide the story.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    /// A goal's piece that passes its check goes to GitHub by itself.
    pub send_checked: bool,
    /// The owner merges from Decurion with one Merge tap.
    pub merge: bool,
    /// Decurion deploys the app with one Deploy tap.
    pub deploy: bool,
    /// The app is Decurion's own code: the running Decurion has to be
    /// updated before a merged change is live.
    pub decurion: bool,
    /// The app keeps a list of protected parts.
    pub protected: bool,
}

impl Default for Rules {
    /// A new app's rules: checked work goes up by itself, nothing else.
    fn default() -> Self {
        Self {
            send_checked: true,
            merge: false,
            deploy: false,
            decurion: false,
            protected: false,
        }
    }
}

impl Rules {
    /// A merged change still has a step to go before it is live.
    pub fn goes_live(&self) -> bool {
        self.deploy || self.decurion
    }
}

/// What the hub knows about one piece of work, for its road.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub thread: ThreadState,
    /// A piece of a goal, so a checker agent tries it: its road has the
    /// Checked step. Work added straight to the board has none.
    pub checker: bool,
    pub review: ReviewState,
    /// Held for the owner because it changes protected parts: the files
    /// named, or empty when the hub could not tell which.
    pub held: Option<String>,
    pub send: Option<SendState>,
    /// Its GitHub link is a pull request, not a page to open one.
    pub pull_request: bool,
    /// It was sent to GitHub, not to some other git server.
    pub on_github: bool,
    pub branch: Option<String>,
    pub merged: bool,
    /// Its pull request was closed on GitHub without merging.
    pub closed: bool,
    pub live: bool,
    /// The hub's saved plain reason (`gh_error`): why the push failed, or
    /// why no pull request was opened.
    pub error: Option<String>,
}

const STEPS: [Step; 6] = [
    Step::Built,
    Step::Checked,
    Step::Pushed,
    Step::PullRequest,
    Step::Merged,
    Step::Live,
];

fn label(step: Step, r: &Rules) -> &'static str {
    match step {
        Step::Built => "Built",
        Step::Checked => "Checked",
        Step::Pushed => "Pushed",
        Step::PullRequest => "Pull request",
        Step::Merged => "Merged",
        // The same name whether it is done or not: what the step is, never
        // what the owner has to do for it.
        Step::Live if r.decurion => "On your PC",
        Step::Live => "Live",
    }
}

/// Whether `step` is on the road of work: Checked only for a goal's piece
/// (`checker`), Live only for an app with a step after the merge.
fn applies(step: Step, checker: bool, r: &Rules) -> bool {
    match step {
        Step::Checked => checker,
        Step::Live => r.goes_live(),
        _ => true,
    }
}

/// The steps that apply to a goal's pieces in the app, all still to do.
fn plain_steps(r: &Rules) -> Vec<RoadStep> {
    STEPS
        .iter()
        .filter(|s| applies(**s, true, r))
        .map(|&step| RoadStep {
            step,
            label: label(step, r).to_owned(),
            mark: StepMark::Todo,
        })
        .collect()
}

/// How to update the Decurion on this PC, one line per way it may run.
pub fn update_lines() -> Vec<String> {
    vec![
        "Installed with the installer: install the new version of Decurion once it is released, then start Decurion again.".to_owned(),
        "Running from your code folder: get the new code (in GitHub Desktop, Fetch origin, then Pull origin), then rebuild and restart Decurion from that folder.".to_owned(),
        "This says On your PC by itself once the Decurion you run has the change.".to_owned(),
    ]
}

fn road(steps: Vec<RoadStep>, label: &str, next: String, you: bool, done: bool) -> Road {
    Road {
        steps,
        label: label.to_owned(),
        next,
        you,
        done,
        update: Vec::new(),
        why: None,
    }
}

/// Where one piece of work that changes code is, and what happens next. A
/// goal's piece (`f.checker`) has the Checked step; other work does not.
pub fn piece_road(f: &Facts, r: &Rules) -> Road {
    let sent = f.send == Some(SendState::Sent);
    // Sent to a git server that is not GitHub: Decurion cannot see a pull
    // request or a merge there, so the road ends with the push.
    if sent && !f.on_github {
        let mut steps = vec![RoadStep {
            step: Step::Built,
            label: label(Step::Built, r).into(),
            mark: StepMark::Done,
        }];
        if f.checker {
            steps.push(RoadStep {
                step: Step::Checked,
                label: label(Step::Checked, r).into(),
                mark: checked_mark(f),
            });
        }
        steps.push(RoadStep {
            step: Step::Pushed,
            label: label(Step::Pushed, r).into(),
            mark: StepMark::Done,
        });
        let branch = f.branch.as_deref().unwrap_or("its own branch");
        let close = if f.checker {
            " Decurion cannot see that server, so close the goal yourself once it is merged."
        } else {
            " Decurion cannot see that server, so it can't tell when it is merged."
        };
        return road(
            steps,
            "Pushed · merge it on your git server",
            format!(
                "It is on your app's git server as the branch {branch}. Merge it there.{close}"
            ),
            true,
            true,
        );
    }
    // A later step done means every one before it is done too, whatever
    // the thread says now.
    let merged = f.merged || f.live;
    let pull_request = (sent && (f.pull_request || f.closed)) || merged;
    let sent = sent || pull_request;
    let checked = f.review == ReviewState::Passed || f.held.is_some();
    let built = matches!(f.thread, ThreadState::InReview | ThreadState::Done) || sent || checked;
    let done = |s: Step| match s {
        Step::Built => built,
        Step::Checked => checked,
        Step::Pushed => sent,
        Step::PullRequest => pull_request,
        Step::Merged => merged,
        Step::Live => f.live,
    };
    let mut at = None;
    let steps: Vec<RoadStep> = STEPS
        .iter()
        .filter(|s| applies(**s, f.checker, r))
        .map(|&step| {
            let mark =
                if step == Step::Checked && !checked && (f.thread == ThreadState::Done || sent) {
                    StepMark::Skipped
                } else if done(step) {
                    StepMark::Done
                } else if at.is_none() {
                    at = Some(step);
                    StepMark::Now
                } else {
                    StepMark::Todo
                };
            RoadStep {
                step,
                label: label(step, r).to_owned(),
                mark,
            }
        })
        .collect();
    let mut out = match at {
        Some(Step::Built) => match (f.thread, f.review) {
            (ThreadState::NeedsYou, _) => road(
                steps,
                "Needs you · answer the agent",
                "The agent stopped to ask you something. Answer it, and it carries on.".into(),
                true,
                false,
            ),
            (ThreadState::Failed, _) => road(
                steps,
                "Stopped",
                "It could not be finished. Retry it, or tell its agent what to change.".into(),
                true,
                false,
            ),
            (_, ReviewState::Fixing) => road(
                steps,
                "Fixing the checker's notes",
                "The checker sent it back with notes, and its agent is fixing them. Then the checker looks again. You don't need to do anything yet.".into(),
                false,
                false,
            ),
            (ThreadState::Queued, _) => road(
                steps,
                "Waiting to start",
                "It waits for an agent to start building it. You don't need to do anything yet."
                    .into(),
                false,
                false,
            ),
            _ => road(
                steps,
                "Building",
                "An agent is building it. Then a checker agent tries it. You don't need to do anything yet.".into(),
                false,
                false,
            ),
        },
        Some(Step::Checked) => match f.review {
            ReviewState::NeedsALook | ReviewState::Failed => road(
                steps,
                "Needs you · take a look",
                "The checker could not pass it. Look it over, then tap Accept, Send back with your notes, or Reject.".into(),
                true,
                false,
            ),
            _ => {
                let then = if r.send_checked {
                    "If it passes, it goes to GitHub by itself, because you approved the plan."
                } else {
                    "If it passes, it waits for you to tap Accept, as this app's rules say."
                };
                road(
                    steps,
                    "Being checked",
                    format!("A checker agent tries it next. You don't need to do anything yet. {then}"),
                    false,
                    false,
                )
            }
        },
        Some(Step::Pushed) => match f.send {
            Some(SendState::Waiting | SendState::Sending) => road(
                steps,
                "Pushing to GitHub",
                "Decurion is pushing it to GitHub as a branch. You don't need to do anything yet."
                    .into(),
                false,
                false,
            ),
            Some(SendState::Failed) => Road {
                why: f.error.clone(),
                ..road(
                    steps,
                    "Needs you · not pushed",
                    "It did not reach GitHub. See why below, then tap Try again. The work is kept."
                        .into(),
                    true,
                    false,
                )
            },
            _ if f.held.is_some() => {
                let files = f
                    .held
                    .as_deref()
                    .filter(|h| !h.is_empty())
                    .map(|h| format!(" ({h})"))
                    .unwrap_or_default();
                road(
                    steps,
                    "Needs you · tap Accept",
                    format!("It passed its check, but it changes protected parts of the app{files}, so approving the plan does not cover it. Look it over, then tap Accept to send it to GitHub."),
                    true,
                    false,
                )
            }
            _ if f.thread == ThreadState::InReview && !r.send_checked => road(
                steps,
                "Needs you · tap Accept",
                "It passed its check. This app's rules say checked work waits for you, so tap Accept to send it to GitHub.".into(),
                true,
                false,
            ),
            _ => road(
                steps,
                "Pushing to GitHub",
                "It passed its check and is on its way to GitHub.".into(),
                false,
                false,
            ),
        },
        Some(Step::PullRequest) => Road {
            why: f.error.clone(),
            ..road(
                steps,
                "Needs you · open the pull request",
                "It is on GitHub as a branch, but no pull request is open, so it can't be merged yet. Tap Open the pull request. Decurion then watches it and sees the merge by itself.".into(),
                true,
                false,
            )
        },
        Some(Step::Merged) if f.closed => road(
            steps,
            "Closed · not merged",
            "Its pull request was closed on GitHub without merging, so this change is not in your app. Ask for it again if you still want it.".into(),
            false,
            false,
        ),
        Some(Step::Merged) if r.merge => road(
            steps,
            "Needs you · merge it",
            "It is on GitHub as a pull request. Tap Merge on it under Today on the board, or in your phone's inbox. Decurion merges once GitHub's checks pass.".into(),
            true,
            false,
        ),
        Some(Step::Merged) => road(
            steps,
            "Needs you · merge on GitHub",
            "It is on GitHub as a pull request. You or your reviewer merge it there. Decurion sees the merge by itself.".into(),
            true,
            false,
        ),
        Some(Step::Live) if r.decurion => road(
            steps,
            "Merged · not on your PC yet",
            "Merged on GitHub. Your PC still runs the older Decurion, so you won't see this change until Decurion is updated.".into(),
            true,
            false,
        ),
        Some(Step::Live) => road(
            steps,
            "Needs you · deploy",
            "Merged. Tap Deploy on the Ready to deploy card, on the board or your phone, to put it live.".into(),
            true,
            false,
        ),
        None if r.decurion => road(
            steps,
            "On your PC",
            "Running on your PC. Nothing more to do.".into(),
            false,
            true,
        ),
        None if r.deploy => road(steps, "Live", "Live. Nothing more to do.".into(), false, true),
        None => road(
            steps,
            "Merged",
            "Merged into your app's main code. Decurion's part is done.".into(),
            false,
            true,
        ),
    };
    if at == Some(Step::Live) && r.decurion {
        out.update = update_lines();
    }
    out
}

fn checked_mark(f: &Facts) -> StepMark {
    if f.review == ReviewState::Passed || f.held.is_some() {
        StepMark::Done
    } else {
        StepMark::Skipped
    }
}

/// How many steps are behind a road: the index of its step now, or all of
/// them once it is done.
fn progress(r: &Road) -> usize {
    r.steps
        .iter()
        .position(|s| s.mark == StepMark::Now)
        .unwrap_or(r.steps.len())
}

/// Where a whole goal is: the steps of its piece furthest behind, and the
/// next move of the first piece that waits for the owner, if any, else of
/// that piece. `pieces` are the goal's pieces that change code and are
/// still on their way (titles and roads), in plan order; `planned` says a
/// plan with such pieces exists, so a goal waiting for its approval still
/// shows the road ahead.
pub fn goal_road(
    state: GoalState,
    pieces: &[(String, Road)],
    planned: bool,
    r: &Rules,
) -> Option<Road> {
    if pieces.is_empty() {
        let next = match state {
            GoalState::Talking if planned => "Nothing is built until you approve a plan.",
            GoalState::PlanReady if planned => "Nothing is built until you approve the plan.",
            _ => return None,
        };
        return Some(road(
            plain_steps(r),
            "",
            next.into(),
            state == GoalState::PlanReady,
            false,
        ));
    }
    let behind = pieces
        .iter()
        .min_by_key(|(_, road)| progress(road))
        .map(|(_, road)| road)
        .expect("pieces is not empty");
    let (title, lead) = pieces
        .iter()
        .find(|(_, road)| road.you && !road.done)
        .or_else(|| pieces.iter().find(|(_, road)| std::ptr::eq(road, behind)))
        .expect("pieces is not empty");
    let mut next = if pieces.len() > 1 && !behind.done {
        format!("“{title}”: {}", lead.next)
    } else {
        lead.next.clone()
    };
    let mut you = lead.you;
    let mut label = lead.label.clone();
    match state {
        GoalState::Stopped => {
            next = "This goal was stopped.".into();
            you = false;
            label = "Stopped".into();
        }
        GoalState::Done if !behind.done => {
            next = format!("You closed this goal. {next}");
            you = false;
            label = "Closed".into();
        }
        _ => {}
    }
    Some(Road {
        steps: behind.steps.clone(),
        label,
        next,
        you,
        done: behind.done,
        update: lead.update.clone(),
        why: lead.why.clone(),
    })
}

/// The whole way from the plan's approval to live, for this app's rules:
/// what the plan and its approval say will happen. `protected` names the
/// planned pieces expected to change a protected part.
pub fn story(r: &Rules, protected: &[String]) -> String {
    let mut s = vec![
        "Each piece is built by its own agent in a sealed box, then a checker agent tries it."
            .to_owned(),
    ];
    if r.send_checked {
        s.push("Each piece that passes goes to GitHub by itself as a pull request, a proposed change to your app's code. You are not asked again.".into());
        if r.protected {
            s.push(match protected {
                [] => "No piece is expected to change a protected part of the app. One that does waits for your Accept.".to_owned(),
                [one] => format!("“{one}” may change a protected part of the app, so it waits for your Accept."),
                more => format!(
                    "{} may change protected parts of the app, so they wait for your Accept.",
                    more.iter().map(|t| format!("“{t}”")).collect::<Vec<_>>().join(" and ")
                ),
            });
        }
    } else {
        s.push("Each piece that passes waits for you to tap Accept, as this app's rules say. Then it goes to GitHub as a pull request, a proposed change to your app's code.".into());
    }
    s.push(if r.merge {
        "You merge each one with one Merge tap in Decurion, once GitHub's checks pass. Merging puts it into your app's main code.".into()
    } else {
        "Then you or your reviewer merge each one on GitHub. Merging puts it into your app's main code. Decurion never merges by itself.".into()
    });
    s.push(
        if r.decurion {
            "This Decurion keeps running its old version until you update it."
        } else if r.deploy {
            "Then one Deploy tap in Decurion puts it live."
        } else {
            "Decurion's part ends with the merge."
        }
        .into(),
    );
    s.push(
        if r.decurion {
            "The goal closes by itself once the Decurion you run has every change."
        } else if r.deploy {
            "The goal closes by itself once every piece is live."
        } else {
            "The goal closes by itself once every piece is merged."
        }
        .into(),
    );
    s.join(" ")
}

// ---------- What an app's clone says ----------

/// What an app's clone says about the app, read-only.
#[derive(Debug, Clone, Copy, Default)]
struct CloneLook {
    decurion: bool,
    protected: bool,
}

fn looks() -> &'static Mutex<HashMap<String, (Instant, CloneLook)>> {
    static LOOKS: std::sync::OnceLock<Mutex<HashMap<String, (Instant, CloneLook)>>> =
        std::sync::OnceLock::new();
    LOOKS.get_or_init(Mutex::default)
}

/// Asks the clone at `repo_path` (read-only) whether it is Decurion's own
/// code and whether it keeps a list of protected parts. A folder git cannot
/// read is neither. Kept for [`LOOK_SECS`], as the screens ask often.
async fn look_at(repo_path: &str) -> CloneLook {
    if let Some((at, c)) = looks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(repo_path)
    {
        if at.elapsed() < Duration::from_secs(LOOK_SECS) {
            return *c;
        }
    }
    let dir = Path::new(repo_path);
    let has = |file: &str| {
        let spec = format!("HEAD:{file}");
        async move { git(dir, &["cat-file", "-e", &spec]).await.is_ok() }
    };
    let c = CloneLook {
        decurion: has(DECURION_MARK).await,
        protected: has(crate::protected::LIST_FILE).await,
    };
    looks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(repo_path.to_owned(), (Instant::now(), c));
    c
}

/// Whether the app at `repo_path` is Decurion's own code.
pub async fn is_decurion(repo_path: &str) -> bool {
    look_at(repo_path).await.decurion
}

fn sha(s: &str) -> bool {
    (40..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Reads one piece of work's facts from a row holding the thread's
/// `state`, the piece's `review` and `review_summary` (null for work that
/// is no piece of a goal), the thread's `gh_*` columns, the piece's
/// `merged_at`, its pull request's `live_at`, and its pull request's state
/// as `gh_pr_state`.
pub(crate) fn facts_from(row: &sqlx::sqlite::SqliteRow) -> Result<Facts> {
    let state: String = row.try_get("state")?;
    let review: Option<String> = row.try_get("review")?;
    let summary: Option<String> = row.try_get("review_summary")?;
    let send: Option<String> = row.try_get("gh_state")?;
    let url: Option<String> = row.try_get("gh_url")?;
    let pr_state: Option<String> = row.try_get("gh_pr_state")?;
    Ok(Facts {
        thread: ThreadState::from_str(&state).map_err(|e| StoreError::Invalid(e.to_string()))?,
        checker: review.is_some(),
        held: review
            .as_deref()
            .is_some_and(|r| crate::protected::is_held(Some(r), summary.as_deref()))
            .then(|| crate::protected::held_files(summary.as_deref().unwrap_or("")))
            .map(|f| f.unwrap_or_default()),
        review: review
            .as_deref()
            .map(ReviewState::from_str)
            .transpose()
            .map_err(|e| StoreError::Invalid(e.to_string()))?
            .unwrap_or(ReviewState::Building),
        send: send
            .map(|s| SendState::from_str(&s).map_err(StoreError::Invalid))
            .transpose()?,
        pull_request: row.try_get("gh_pr")?,
        on_github: url
            .as_deref()
            .is_some_and(|u| u.starts_with("https://github.com/")),
        branch: row.try_get("gh_branch")?,
        merged: row.try_get::<Option<String>, _>("merged_at")?.is_some()
            || pr_state.as_deref() == Some("merged"),
        closed: pr_state.as_deref() == Some("closed"),
        live: row.try_get::<Option<String>, _>("live_at")?.is_some(),
        error: row.try_get("gh_error")?,
    })
}

/// The columns [`facts_from`] reads, for a query over `threads t` joined
/// (or left joined, for work that is no piece) to `goal_pieces p` and,
/// left joined, its pull request's watch `pr_watches w` (on
/// the work's own id, or for a fix once it was pushed, the pull request it
/// answers: a fix not pushed yet never takes that pull request's merge).
pub(crate) const FACT_COLS: &str = "t.state, p.review, p.review_summary, t.gh_state, t.gh_url,
    t.gh_pr, t.gh_branch, t.gh_error, p.merged_at, w.live_at, w.state AS gh_pr_state";

impl Store {
    /// The rules that decide app `app_id`'s story.
    pub async fn road_rules(&self, app_id: i64) -> Result<Rules> {
        let app = self.app(app_id).await?;
        let clone = look_at(&app.repo_path).await;
        Ok(Rules {
            send_checked: app.rules.send_checked,
            merge: app.rules.merge,
            deploy: app.rules.deploy_steps().is_some(),
            decurion: clone.decurion,
            protected: clone.protected || app.rules.deploy_steps().is_some(),
        })
    }

    /// The road of piece `thread_id`, or `None` when it is no piece of a
    /// goal or is research.
    pub async fn piece_road(&self, thread_id: i64, r: &Rules) -> Result<Option<Road>> {
        let row = sqlx::query(&format!(
            "SELECT t.research, {FACT_COLS} FROM goal_pieces p JOIN threads t ON t.id = p.thread_id
             LEFT JOIN pr_watches w ON w.thread_id = CASE WHEN t.fix_of IS NULL THEN t.id WHEN t.gh_state = 'sent' THEN t.fix_of END
             WHERE p.thread_id = ?"
        ))
        .bind(thread_id)
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else { return Ok(None) };
        if row.try_get::<bool, _>("research")? {
            return Ok(None);
        }
        Ok(Some(piece_road(&facts_from(&row)?, r)))
    }

    /// The roads of finished work that went to GitHub (state done, not
    /// research, with a send), by thread: `only` one thread, or all of
    /// them. A goal's piece gets exactly its piece road; other work the
    /// same road without the Checked step. One read for the facts, and the
    /// rules asked once per app.
    pub async fn work_roads(&self, only: Option<i64>) -> Result<HashMap<i64, Road>> {
        let rows = sqlx::query(&format!(
            "SELECT t.id, t.app_id, {FACT_COLS} FROM threads t
             LEFT JOIN goal_pieces p ON p.thread_id = t.id
             LEFT JOIN pr_watches w ON w.thread_id = CASE WHEN t.fix_of IS NULL THEN t.id WHEN t.gh_state = 'sent' THEN t.fix_of END
             WHERE t.state = 'done' AND t.research = 0 AND t.gh_state IS NOT NULL
               AND (?1 IS NULL OR t.id = ?1)"
        ))
        .bind(only)
        .fetch_all(self.pool())
        .await?;
        let mut rules: HashMap<i64, Rules> = HashMap::new();
        let mut out = HashMap::new();
        for row in &rows {
            let app: i64 = row.try_get("app_id")?;
            let r = match rules.get(&app) {
                Some(r) => *r,
                None => {
                    let r = self.road_rules(app).await?;
                    rules.insert(app, r);
                    r
                }
            };
            out.insert(row.try_get("id")?, piece_road(&facts_from(row)?, &r));
        }
        Ok(out)
    }

    /// Fills in the road of each finished thread that went to GitHub, for
    /// a screen ([`Store::work_roads`]).
    pub async fn with_roads<'a, I>(&self, threads: I) -> Result<()>
    where
        I: IntoIterator<Item = &'a mut decurion_proto::Thread>,
    {
        let mut threads: Vec<&mut decurion_proto::Thread> = threads
            .into_iter()
            .filter(|t| t.state == ThreadState::Done && t.github.is_some() && !t.research)
            .collect();
        if threads.is_empty() {
            return Ok(());
        }
        let only = match threads.as_slice() {
            [one] => Some(one.id),
            _ => None,
        };
        let mut roads = self.work_roads(only).await?;
        for t in threads.iter_mut() {
            t.road = roads.remove(&t.id);
        }
        Ok(())
    }

    /// One thread as a screen shows it, with its road when it has one.
    pub async fn thread_with_road(&self, id: i64) -> Result<decurion_proto::Thread> {
        let mut t = self.thread(id).await?;
        self.with_roads([&mut t]).await?;
        Ok(t)
    }

    /// For Decurion's own code: marks Live each merged pull request whose
    /// merge commit the running hub was built from (`hub_commit`, from
    /// [`crate::HUB_COMMIT`]), asking the app's clone with read-only git.
    /// Closes the goals that leaves with nothing to do, and returns their
    /// phone notices. A hub that does not know its commit marks nothing.
    pub async fn mark_self_live(&self, hub_commit: &str) -> Result<Vec<Notice>> {
        if !sha(hub_commit) {
            return Ok(Vec::new());
        }
        let rows: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT w.thread_id, w.merge_commit, a.repo_path FROM pr_watches w
             JOIN apps a ON a.id = w.app_id
             WHERE w.state = 'merged' AND w.live_at IS NULL AND w.merge_commit IS NOT NULL",
        )
        .fetch_all(self.pool())
        .await?;
        let mut news = Vec::new();
        for (thread_id, merge, repo) in rows {
            if !sha(&merge) || !is_decurion(&repo).await {
                continue;
            }
            let held = merge == hub_commit
                || git(
                    Path::new(&repo),
                    &["merge-base", "--is-ancestor", &merge, hub_commit],
                )
                .await
                .is_ok();
            if !held {
                continue;
            }
            sqlx::query(
                "UPDATE pr_watches SET live_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE thread_id = ? AND live_at IS NULL",
            )
            .bind(thread_id)
            .execute(self.pool())
            .await?;
            tracing::info!(
                thread = thread_id,
                "the running Decurion has this merged change"
            );
            if let Some(n) = self.piece_live(thread_id).await? {
                news.push(n);
            }
        }
        Ok(news)
    }
}

/// Marks Decurion's own merged changes Live once this hub runs them, and
/// tells the owner's phone about goals that closed. Called at start and
/// after each look at pull requests.
pub async fn notice_self_live(state: &crate::AppState) {
    match state.store.mark_self_live(crate::HUB_COMMIT).await {
        Ok(news) => {
            if let Some(p) = &state.push {
                for n in &news {
                    p.send(&state.store, n).await;
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not tell whether this Decurion runs its merged changes")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(thread: ThreadState, review: ReviewState) -> Facts {
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
            ..facts(ThreadState::Done, ReviewState::Passed)
        }
    }

    /// Work added straight to the board: no goal, so no checker.
    fn board_work(f: Facts) -> Facts {
        Facts {
            checker: false,
            review: ReviewState::Building,
            ..f
        }
    }

    fn now(r: &Road) -> Option<&str> {
        r.steps
            .iter()
            .find(|s| s.mark == StepMark::Now)
            .map(|s| s.label.as_str())
    }

    fn labels(r: &Road) -> Vec<&str> {
        r.steps.iter().map(|s| s.label.as_str()).collect()
    }

    fn me() -> Rules {
        Rules {
            decurion: true,
            ..Rules::default()
        }
    }

    #[test]
    fn the_steps_are_the_same_words_and_live_only_where_it_applies() {
        let plain = Rules::default();
        let f = facts(ThreadState::Running, ReviewState::Building);
        assert_eq!(
            labels(&piece_road(&f, &plain)),
            ["Built", "Checked", "Pushed", "Pull request", "Merged"]
        );
        let deploys = Rules {
            deploy: true,
            ..plain
        };
        assert_eq!(
            labels(&piece_road(&f, &deploys)),
            [
                "Built",
                "Checked",
                "Pushed",
                "Pull request",
                "Merged",
                "Live"
            ]
        );
        // Decurion's own code: the last step is always "On your PC", never
        // what the owner has to do.
        assert_eq!(
            labels(&piece_road(&f, &me())),
            [
                "Built",
                "Checked",
                "Pushed",
                "Pull request",
                "Merged",
                "On your PC"
            ]
        );
        assert_eq!(now(&piece_road(&f, &plain)), Some("Built"));
        let steps: Vec<Step> = piece_road(&f, &plain)
            .steps
            .iter()
            .map(|s| s.step)
            .collect();
        assert_eq!(
            steps,
            [
                Step::Built,
                Step::Checked,
                Step::Pushed,
                Step::PullRequest,
                Step::Merged
            ]
        );
    }

    #[test]
    fn work_that_is_no_piece_of_a_goal_has_no_checked_step() {
        let r = piece_road(&board_work(sent(true)), &Rules::default());
        assert_eq!(labels(&r), ["Built", "Pushed", "Pull request", "Merged"]);
        assert_eq!(now(&r), Some("Merged"));
        assert_eq!(r.label, "Needs you · merge on GitHub");
        assert!(r.steps.iter().all(|s| s.mark != StepMark::Skipped));
    }

    #[test]
    fn while_checking_the_next_line_follows_the_send_rule() {
        let f = facts(ThreadState::InReview, ReviewState::Reviewing);
        let on = piece_road(&f, &Rules::default());
        assert_eq!(now(&on), Some("Checked"));
        assert!(!on.you);
        assert!(on.next.contains("goes to GitHub by itself"), "{}", on.next);
        let off = piece_road(
            &f,
            &Rules {
                send_checked: false,
                ..Rules::default()
            },
        );
        assert!(
            off.next.contains("waits for you to tap Accept"),
            "{}",
            off.next
        );
        assert!(!off.next.contains("by itself"), "{}", off.next);
    }

    #[test]
    fn a_held_piece_names_its_protected_files_and_waits_for_accept() {
        let f = Facts {
            held: Some("src/spend.rs".into()),
            ..facts(ThreadState::InReview, ReviewState::NeedsALook)
        };
        let r = piece_road(&f, &Rules::default());
        assert_eq!(now(&r), Some("Pushed"));
        assert!(r.you);
        assert!(
            r.next.contains("protected parts of the app (src/spend.rs)"),
            "{}",
            r.next
        );
        assert!(r.next.contains("tap Accept"), "{}", r.next);
    }

    #[test]
    fn a_push_on_its_way_or_failed_says_so_and_why() {
        let going = Facts {
            send: Some(SendState::Sending),
            ..facts(ThreadState::Done, ReviewState::Passed)
        };
        let r = piece_road(&going, &Rules::default());
        assert_eq!(now(&r), Some("Pushed"));
        assert_eq!(r.label, "Pushing to GitHub");
        assert!(!r.you && r.why.is_none());

        let failed = Facts {
            send: Some(SendState::Failed),
            error: Some("This app's folder is not linked to GitHub.".into()),
            ..board_work(facts(ThreadState::Done, ReviewState::Passed))
        };
        let r = piece_road(&failed, &Rules::default());
        assert_eq!(now(&r), Some("Pushed"));
        assert_eq!(r.label, "Needs you · not pushed");
        assert!(r.you && !r.done);
        assert_eq!(
            r.why.as_deref(),
            Some("This app's folder is not linked to GitHub.")
        );
    }

    #[test]
    fn pushed_with_no_pull_request_is_not_waiting_to_be_merged() {
        let f = Facts {
            error: Some("Your GitHub token can't open pull requests on this app.".into()),
            ..sent(false)
        };
        let r = piece_road(&f, &Rules::default());
        assert_eq!(now(&r), Some("Pull request"));
        assert_eq!(r.label, "Needs you · open the pull request");
        assert!(r.you && !r.done);
        assert!(
            r.next.starts_with("It is on GitHub as a branch, but no pull request is open, so it can't be merged yet. Tap Open the pull request."),
            "{}",
            r.next
        );
        assert_eq!(
            r.why.as_deref(),
            Some("Your GitHub token can't open pull requests on this app.")
        );
        let marks: Vec<StepMark> = r.steps.iter().map(|s| s.mark).collect();
        assert_eq!(
            marks,
            [
                StepMark::Done,
                StepMark::Done,
                StepMark::Done,
                StepMark::Now,
                StepMark::Todo
            ]
        );
        // Without a saved reason, nothing is said to be stuck.
        assert!(piece_road(&sent(false), &Rules::default()).why.is_none());
    }

    #[test]
    fn on_github_the_merge_is_told_by_the_merge_rule() {
        let r = piece_road(&sent(true), &Rules::default());
        assert_eq!(now(&r), Some("Merged"));
        assert_eq!(r.label, "Needs you · merge on GitHub");
        assert!(r.next.contains("You or your reviewer merge it there"));
        assert!(r.why.is_none());
        let r = piece_road(
            &sent(true),
            &Rules {
                merge: true,
                ..Rules::default()
            },
        );
        assert_eq!(r.label, "Needs you · merge it");
        assert!(r.next.contains("Tap Merge"), "{}", r.next);
    }

    #[test]
    fn a_pull_request_closed_without_merging_is_said_plainly() {
        let f = Facts {
            closed: true,
            ..sent(true)
        };
        let r = piece_road(&f, &Rules::default());
        assert_eq!(r.label, "Closed · not merged");
        assert!(!r.you && !r.done);
        assert_eq!(now(&r), Some("Merged"));
        assert!(
            r.next.contains("closed on GitHub without merging"),
            "{}",
            r.next
        );
        assert_eq!(
            r.steps[3].mark,
            StepMark::Done,
            "the pull request was there"
        );
    }

    #[test]
    fn merged_is_the_end_unless_the_app_deploys_or_is_decurion() {
        let merged = Facts {
            merged: true,
            ..sent(true)
        };
        let r = piece_road(&merged, &Rules::default());
        assert!(r.done);
        assert_eq!(r.label, "Merged");
        assert!(r.steps.iter().all(|s| s.mark == StepMark::Done));

        let r = piece_road(&merged, &me());
        assert!(!r.done && r.you);
        assert_eq!(now(&r), Some("On your PC"));
        assert_eq!(r.label, "Merged · not on your PC yet");
        assert_eq!(
            r.next,
            "Merged on GitHub. Your PC still runs the older Decurion, so you won't see this change until Decurion is updated."
        );
        assert_eq!(r.update.len(), 3);
        let live = piece_road(
            &Facts {
                live: true,
                ..merged.clone()
            },
            &me(),
        );
        assert!(live.done && !live.you);
        assert_eq!(live.label, "On your PC");
        assert_eq!(live.next, "Running on your PC. Nothing more to do.");
        assert_eq!(
            live.steps.last().map(|s| s.label.as_str()),
            Some("On your PC")
        );
        assert!(live.update.is_empty());

        let deploys = Rules {
            deploy: true,
            ..Rules::default()
        };
        let r = piece_road(&merged, &deploys);
        assert_eq!(r.label, "Needs you · deploy");
        assert!(r.next.contains("Tap Deploy"), "{}", r.next);
        let r = piece_road(
            &Facts {
                live: true,
                ..merged
            },
            &deploys,
        );
        assert!(r.done);
        assert_eq!(r.label, "Live");
    }

    #[test]
    fn work_pushed_to_another_git_server_ends_with_the_push() {
        let f = Facts {
            on_github: false,
            ..sent(false)
        };
        let r = piece_road(&f, &Rules::default());
        assert_eq!(labels(&r), ["Built", "Checked", "Pushed"]);
        assert!(r.done);
        assert!(r.next.contains("decurion/x"), "{}", r.next);
        let r = piece_road(&board_work(f), &Rules::default());
        assert_eq!(labels(&r), ["Built", "Pushed"]);
        assert!(!r.next.contains("goal"), "{}", r.next);
    }

    #[test]
    fn a_piece_accepted_before_its_check_shows_the_check_skipped() {
        let f = Facts {
            review: ReviewState::Reviewing,
            ..sent(true)
        };
        let r = piece_road(&f, &Rules::default());
        assert_eq!(r.steps[1].mark, StepMark::Skipped);
    }

    #[test]
    fn a_goal_is_where_its_slowest_piece_is_and_asks_what_waits_for_you() {
        let r = Rules::default();
        let a = piece_road(
            &Facts {
                error: Some("No token.".into()),
                ..sent(false)
            },
            &r,
        );
        let b = piece_road(&facts(ThreadState::Running, ReviewState::Building), &r);
        let g = goal_road(
            GoalState::Working,
            &[("Help link".into(), a), ("Logo".into(), b)],
            true,
            &r,
        )
        .expect("road");
        assert_eq!(now(&g), Some("Built"));
        assert!(g.you);
        assert!(
            g.next.starts_with("“Help link”: It is on GitHub"),
            "{}",
            g.next
        );
        assert_eq!(g.label, "Needs you · open the pull request");
        assert_eq!(g.why.as_deref(), Some("No token."));

        let plan = goal_road(GoalState::PlanReady, &[], true, &r).expect("road");
        assert!(plan.steps.iter().all(|s| s.mark == StepMark::Todo));
        assert!(goal_road(GoalState::PlanReady, &[], false, &r).is_none());
    }

    #[test]
    fn a_road_with_nothing_stuck_has_no_why_in_its_json() {
        let r = piece_road(&sent(true), &Rules::default());
        let v = serde_json::to_value(&r).expect("json");
        assert!(v.get("why").is_none(), "{v}");
        assert_eq!(v["steps"][3]["step"], "pull_request");
        assert_eq!(v["steps"][2]["step"], "pushed");
    }

    #[test]
    fn the_story_is_built_from_the_rules() {
        let r = Rules::default();
        let s = story(&r, &[]);
        assert!(s.contains("goes to GitHub by itself"), "{s}");
        assert!(
            s.contains("you or your reviewer merge each one on GitHub"),
            "{s}"
        );
        assert!(s.contains("ends with the merge"), "{s}");
        assert!(
            !s.contains("protected"),
            "an app with no list hears nothing of it: {s}"
        );
        let me = Rules {
            decurion: true,
            protected: true,
            ..r
        };
        let s = story(&me, &["Spend cap".into()]);
        assert!(s.contains("“Spend cap” may change a protected part"), "{s}");
        assert!(s.contains("until you update it"), "{s}");
        let s = story(
            &Rules {
                send_checked: false,
                merge: true,
                deploy: true,
                ..r
            },
            &[],
        );
        assert!(s.contains("waits for you to tap Accept"), "{s}");
        assert!(s.contains("one Merge tap"), "{s}");
        assert!(s.contains("one Deploy tap"), "{s}");
        assert!(!s.contains('\u{2014}'), "no long dashes");
    }
}
