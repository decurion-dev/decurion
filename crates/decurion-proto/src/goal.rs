//! Goals and the coordinator.
//!
//! The owner gives a goal in plain words. The coordinator, a read-only Claude
//! Code agent, interviews the owner with multiple-choice questions, proposes a
//! plan split into pieces of work, and after one approval each piece runs as
//! an ordinary thread. When a piece's agent finishes, a second agent reviews
//! it and can send it back with notes. When every piece is settled the
//! coordinator writes a plain-words report and the goal waits for the owner.
//!
//! Agents never act on the hub directly. The coordinator and the reviewer only
//! return text; the hub checks it against the shapes below and does the rest.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Where a goal is. Whether the coordinator is thinking right now is
/// [`Goal::busy`], not a state, because the owner can talk to it at any time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalState {
    /// The coordinator and the owner are talking it through.
    Talking,
    /// A plan is waiting for the owner's approval.
    PlanReady,
    /// Pieces of work are running or being reviewed.
    Working,
    /// Everything is built and reviewed; the owner looks at it.
    ForReview,
    /// The owner closed it.
    Done,
    /// The owner stopped it.
    Stopped,
}

impl GoalState {
    pub const ALL: [GoalState; 6] = [
        Self::Talking,
        Self::PlanReady,
        Self::Working,
        Self::ForReview,
        Self::Done,
        Self::Stopped,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Talking => "talking",
            Self::PlanReady => "plan_ready",
            Self::Working => "working",
            Self::ForReview => "for_review",
            Self::Done => "done",
            Self::Stopped => "stopped",
        }
    }

    /// Closed goals take no more messages.
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Done | Self::Stopped)
    }
}

impl fmt::Display for GoalState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for GoalState {
    type Err = crate::UnknownState;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|st| st.as_str() == s)
            .ok_or_else(|| crate::UnknownState(s.to_owned()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Goal {
    pub id: i64,
    pub app_id: i64,
    pub title: String,
    pub state: GoalState,
    /// The coordinator is working on a reply.
    pub busy: bool,
    /// The last coordinator turn failed; the owner can try again.
    pub stuck: bool,
    pub created_at: String,
    pub updated_at: String,
    /// When the owner dismissed this closed goal from the goal list.
    #[serde(default)]
    pub dismissed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewGoal {
    pub app_id: i64,
    /// What the owner wants, in their own words.
    pub text: String,
}

/// A question with choices, the way a project lead asks a client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Question {
    /// Short id the coordinator picks, such as `q1`.
    pub id: String,
    pub text: String,
    pub choices: Vec<String>,
    /// Index into `choices` of the coordinator's suggestion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanPiece {
    pub title: String,
    /// Full instructions for the agent that builds it.
    pub brief: String,
    /// How the owner will know it works, in plain words.
    pub done_when: String,
    /// The project lead expects this piece to change a protected part of
    /// the app, so the owner knows up front it will wait for their one tap
    /// at the end. Only a forecast: whether a piece is held is decided by
    /// the hub from what the work actually changed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub touches_protected: bool,
    /// A research piece: its agent finds things out and writes a report
    /// page instead of changing code. Nothing from it goes to GitHub.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub research: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    /// What will be built, in plain words.
    pub summary: String,
    pub pieces: Vec<PlanPiece>,
}

/// What the coordinator returns each turn, as the JSON object at the end of
/// its answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CoordinatorReply {
    /// What it says to the owner, in plain words.
    pub say: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<Question>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Plan>,
    /// Facts about the app worth keeping for later goals.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remember: Vec<String>,
    /// Items for the app's master plan, under this goal: what finished work
    /// found, or follow-ups worth doing. The owner decides what happens to
    /// them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todo: Vec<crate::plan::LeadTodo>,
}

pub const MAX_QUESTIONS: usize = 4;
pub const MAX_CHOICES: usize = 5;
pub const MAX_PIECES: usize = 6;
pub const MAX_REMEMBER: usize = 5;

fn check_len(what: &str, s: &str, max: usize) -> Result<(), String> {
    if s.trim().is_empty() {
        return Err(format!("{what} is empty"));
    }
    if s.chars().count() > max {
        return Err(format!("{what} is longer than {max} characters"));
    }
    Ok(())
}

impl CoordinatorReply {
    /// Checks the shape the hub relies on. The message names what is wrong,
    /// so it can be handed back to the coordinator to fix.
    pub fn validate(&self) -> Result<(), String> {
        check_len("say", &self.say, 4000)?;
        if !self.questions.is_empty() && self.plan.is_some() {
            return Err("ask questions or propose a plan, not both".into());
        }
        if self.questions.len() > MAX_QUESTIONS {
            return Err(format!("at most {MAX_QUESTIONS} questions at a time"));
        }
        let mut ids = std::collections::HashSet::new();
        for q in &self.questions {
            check_len("question id", &q.id, 20)?;
            if !ids.insert(q.id.as_str()) {
                return Err(format!("question id {} is used twice", q.id));
            }
            check_len("question text", &q.text, 500)?;
            if !(2..=MAX_CHOICES).contains(&q.choices.len()) {
                return Err(format!(
                    "question {} needs 2 to {MAX_CHOICES} choices",
                    q.id
                ));
            }
            for c in &q.choices {
                check_len("choice", c, 200)?;
            }
            if q.recommended.is_some_and(|r| r >= q.choices.len()) {
                return Err(format!(
                    "question {} recommends a choice it does not have",
                    q.id
                ));
            }
        }
        if let Some(p) = &self.plan {
            check_len("plan summary", &p.summary, 2000)?;
            if !(1..=MAX_PIECES).contains(&p.pieces.len()) {
                return Err(format!("a plan needs 1 to {MAX_PIECES} pieces"));
            }
            for piece in &p.pieces {
                check_len("piece title", &piece.title, 80)?;
                check_len("piece brief", &piece.brief, 8000)?;
                check_len("piece done_when", &piece.done_when, 1000)?;
            }
        }
        if self.remember.len() > MAX_REMEMBER {
            return Err(format!("remember at most {MAX_REMEMBER} things at a time"));
        }
        for r in &self.remember {
            check_len("remember entry", r, 300)?;
        }
        crate::plan::validate_todo(&self.todo)?;
        Ok(())
    }
}

/// The owner's answer to one question: a choice, their own words, or both.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Answer {
    pub question_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<String>,
}

/// One entry in a goal's conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GoalMessageBody {
    /// The owner wrote something.
    Owner { text: String },
    /// The owner answered the coordinator's questions.
    Answers {
        /// The coordinator message the answers belong to.
        to: i64,
        answers: Vec<Answer>,
    },
    /// The coordinator replied.
    Coordinator(CoordinatorReply),
    /// The coordinator's report on finished work.
    Report { say: String },
    /// Decurion itself noting what happened (plan approved, review passed).
    Note { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GoalMessage {
    pub id: i64,
    pub goal_id: i64,
    pub at: String,
    pub body: GoalMessageBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerSays {
    pub text: String,
    /// The plan on the owner's screen when they wrote. A plain yes
    /// ([`is_plain_yes`]) approves exactly this plan, if it is still the
    /// latest; without it, a yes is only words to the project lead.
    #[serde(default)]
    pub plan_message_id: Option<i64>,
}

/// Longest message read as a plain yes. Anything longer says more than yes.
const MAX_YES_CHARS: usize = 40;

/// The short ways an owner says "go ahead", compared after lowering case,
/// dropping marks at the ends and squeezing spaces. Kept to a fixed list,
/// so "yes but change the colour" is never read as approval.
const PLAIN_YES: &[&str] = &[
    "yes",
    "y",
    "yep",
    "yeah",
    "yup",
    "ok",
    "okay",
    "k",
    "sure",
    "go",
    "go ahead",
    "go for it",
    "do it",
    "yes do it",
    "ok do it",
    "yes go",
    "yes go ahead",
    "yes please",
    "please do",
    "approve",
    "approved",
    "accept",
    "accepted",
    "push",
    "push it",
    "ship it",
    "send it",
    "lgtm",
    "looks good",
    "sounds good",
    "perfect",
    "great do it",
    "👍",
    "✅",
];

/// True if `text` is nothing more than a yes, such as "Yes, do it!" or
/// "go ahead". The hub treats such a message as the tap it stands for (the
/// waiting plan's Approve, or work's Accept), and only where the owner's
/// screen showed that exact thing.
pub fn is_plain_yes(text: &str) -> bool {
    if text.chars().count() > MAX_YES_CHARS {
        return false;
    }
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '.' | '!' | ';' | ':' | '-'))
        .filter(|w| !w.is_empty())
        .collect();
    let squeezed = words.join(" ");
    PLAIN_YES.contains(&squeezed.as_str())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerAnswers {
    /// The coordinator message whose questions these answer.
    pub to: i64,
    pub answers: Vec<Answer>,
}

/// The owner's one approval for a goal, bound to the exact plan shown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovePlan {
    pub plan_message_id: i64,
}

/// Where a piece of work is in the second agent's review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    /// The building agent is still working.
    Building,
    /// The reviewer is checking it.
    Reviewing,
    /// The reviewer sent it back with notes and the agent is fixing them.
    Fixing,
    /// The reviewer is happy.
    Passed,
    /// Review rounds ran out with problems left; the owner should look.
    NeedsALook,
    /// The building agent failed.
    Failed,
}

impl ReviewState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Building => "building",
            Self::Reviewing => "reviewing",
            Self::Fixing => "fixing",
            Self::Passed => "passed",
            Self::NeedsALook => "needs_a_look",
            Self::Failed => "failed",
        }
    }

    /// Nothing more will happen to it without the owner.
    pub fn is_settled(self) -> bool {
        matches!(self, Self::Passed | Self::NeedsALook | Self::Failed)
    }
}

impl FromStr for ReviewState {
    type Err = crate::UnknownState;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        [
            Self::Building,
            Self::Reviewing,
            Self::Fixing,
            Self::Passed,
            Self::NeedsALook,
            Self::Failed,
        ]
        .into_iter()
        .find(|st| st.as_str() == s)
        .ok_or_else(|| crate::UnknownState(s.to_owned()))
    }
}

/// One piece of a goal's plan and the thread building it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Piece {
    pub thread_id: i64,
    pub goal_id: i64,
    pub title: String,
    pub done_when: String,
    pub thread_state: crate::ThreadState,
    pub review: ReviewState,
    /// Review rounds so far.
    pub rounds: i64,
    /// The reviewer's last summary, in plain words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_issues: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Where the piece is on its way to GitHub, once accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<crate::GitHubSend>,
    /// Its pull request was merged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub merged: bool,
    /// A research piece: it writes a report and changes no code.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub research: bool,
    /// A research piece's report page is ready, at
    /// `/api/goals/{goal_id}/reports/{thread_id}`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub report: bool,
    /// What the building (or researching) agent said when it last finished
    /// well, in its own words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_summary: Option<String>,
    /// What the checker ran in its last check, and what came of it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_evidence: Vec<Evidence>,
    /// The checker's claims in its last check, each marked checked or
    /// guessed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review_claims: Vec<ReviewClaim>,
    /// Tokens this piece used: its building runs and its checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<crate::tokens::TokenLine>,
}

/// One thing the checker actually did, such as running the tests or opening
/// a screen, and what came of it, in plain words.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Evidence {
    /// What it ran or looked at, such as "the app's tests".
    pub ran: String,
    /// What came of it, such as "42 of 42 passed".
    pub result: String,
}

/// How the checker knows a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// It saw it happen: a test, a command, a screen, the code itself.
    Checked,
    /// It reasoned its way there without seeing it.
    Guessed,
}

impl Basis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Checked => "checked",
            Self::Guessed => "guessed",
        }
    }
}

/// One thing the checker says about the work, and how it knows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewClaim {
    pub text: String,
    pub how: Basis,
}

/// The report a research piece's agent left, posted by the runner before it
/// moves the work to review. Without a page of its own the hub makes one
/// from the agent's final summary.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ResearchReport {
    /// A self-contained HTML page, no bigger than [`MAX_PAGE_BYTES`], with
    /// test keys already hidden by the runner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
}

/// The owner asks the project lead why a piece of work was done the way it
/// was. The lead answers from the piece's real change and its agent's log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskWhy {
    /// The piece's thread.
    pub thread_id: i64,
    /// The owner's question, in their own words.
    pub text: String,
}

/// What a visual page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageKind {
    /// The plan drawn out: mockups, and for changes, today's screens beside
    /// the planned ones.
    Plan,
    /// A finished piece as built: screenshots taken by the checker.
    Check,
}

impl PageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Check => "check",
        }
    }
}

/// A self-contained HTML page an agent drew. Opened at
/// `/api/goals/{goal_id}/pages/{id}` in a sandbox that runs no scripts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PageInfo {
    pub id: i64,
    pub kind: PageKind,
    /// For a plan page: the coordinator message holding that plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i64>,
    /// For a check page: the piece's thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<i64>,
    pub created_at: String,
}

/// Everything the goal page shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalView {
    pub goal: Goal,
    pub app_name: String,
    pub messages: Vec<GoalMessage>,
    pub pieces: Vec<Piece>,
    pub pages: Vec<PageInfo>,
    /// The plan page for the latest plan is being drawn.
    pub drawing: bool,
    /// For an app that deploys: whether every merged pull request of the goal
    /// is in a release yet ("Live") or not ("Merged · ready to deploy").
    /// Absent for apps that do not deploy and goals with nothing merged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<bool>,
    /// Tokens the goal used so far: its pieces' work and checks, and the
    /// project lead's turns. `None` until an agent reported any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<GoalUsage>,
}

/// A goal's tokens, in total and by who used them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalUsage {
    pub total: crate::tokens::TokenLine,
    /// The project lead: talking, planning, drawing pages, summing up.
    pub lead: crate::tokens::TokenLine,
    /// Every piece's building and its checks.
    pub pieces: crate::tokens::TokenLine,
}

/// What the reviewer returns, as the JSON object at the end of its answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewVerdict {
    pub pass: bool,
    /// What it found, in plain words.
    pub summary: String,
    /// Problems the building agent must fix. Empty when `pass`.
    #[serde(default)]
    pub issues: Vec<String>,
    /// What the checker actually ran or looked at, and what came of it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
    /// What it claims about the work, each marked checked or guessed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<ReviewClaim>,
    /// Questions for the owner about the checker's page, answered beside it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<Question>,
}

pub const MAX_EVIDENCE: usize = 10;
pub const MAX_CLAIMS: usize = 10;

impl ReviewVerdict {
    pub fn validate(&self) -> Result<(), String> {
        check_len("summary", &self.summary, 2000)?;
        if !self.pass && self.issues.is_empty() {
            return Err("a failing review must list the issues to fix".into());
        }
        if self.issues.len() > 10 {
            return Err("list at most 10 issues".into());
        }
        for i in &self.issues {
            check_len("issue", i, 1000)?;
        }
        if self.evidence.len() > MAX_EVIDENCE {
            return Err(format!("list at most {MAX_EVIDENCE} evidence entries"));
        }
        for e in &self.evidence {
            check_len("evidence ran", &e.ran, 300)?;
            check_len("evidence result", &e.result, 500)?;
        }
        if self.claims.len() > MAX_CLAIMS {
            return Err(format!("list at most {MAX_CLAIMS} claims"));
        }
        for c in &self.claims {
            check_len("claim", &c.text, 500)?;
        }
        crate::plan::validate_page_questions(&self.questions)?;
        Ok(())
    }
}

/// Finds the last top-level JSON object in an agent's answer, whether bare or
/// in a fenced code block, and reads it as `T`.
pub fn last_json_object<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    let mut last_err = "no JSON object found".to_owned();
    // Try each `{` from the end, so prose before the object is ignored and the
    // final object wins over examples quoted earlier.
    let starts: Vec<usize> = text.match_indices('{').map(|(i, _)| i).collect();
    for &start in starts.iter().rev() {
        let mut de = serde_json::Deserializer::from_str(&text[start..]).into_iter::<T>();
        match de.next() {
            Some(Ok(v)) => {
                let rest = text[start + de.byte_offset()..].trim();
                let rest = rest.trim_start_matches("```").trim();
                if rest.is_empty() {
                    return Ok(v);
                }
            }
            Some(Err(e)) if !e.is_syntax() && !e.is_eof() => last_err = e.to_string(),
            _ => {}
        }
    }
    Err(last_err)
}

// ---- runner side ----

/// The helper agents. None of them builds anything the owner ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperKind {
    /// Talks with the owner and plans. Sees the app's code read-only.
    Coordinator,
    /// Draws the plan as a visual page, in its own throwaway copy of the app.
    Design,
    /// Checks a finished piece of work in its own throwaway copy.
    Review,
}

impl HelperKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Coordinator => "coordinator",
            Self::Design => "design",
            Self::Review => "review",
        }
    }

    /// Design and review jobs may leave a page in `/out/page.html`.
    pub fn makes_page(self) -> bool {
        matches!(self, Self::Design | Self::Review)
    }
}

/// Largest page a helper may hand back, screenshots included.
pub const MAX_PAGE_BYTES: usize = 8 << 20;

/// The code a helper sees.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HelperSource {
    /// A read-only copy of the app's code at the goal's start commit, kept
    /// per goal.
    App {
        goal_id: i64,
        /// The goal's start commit: the runner builds the copy from the
        /// hub's bundle of exactly it (`/runner/goals/{id}/base`). `None`
        /// only for a goal from before start commits, copied from the
        /// owner's folder as it is now.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_commit: Option<String>,
    },
    /// A fresh, writable, throwaway copy of the app's code at the goal's
    /// start commit, made the same way as [`HelperSource::App`].
    AppCopy {
        goal_id: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_commit: Option<String>,
    },
    /// A fresh, writable, throwaway copy of a thread's branch, taken from the
    /// runner's own review copy (never from the agent's folder). The commit
    /// the thread started from is the local branch `decurion-base`.
    Branch {
        thread_id: i64,
        branch: String,
        /// The thread's latest saved copy. Work last saved on another
        /// computer is brought into the review copy from it first.
        #[serde(default)]
        checkpoint: Option<i64>,
    },
}

/// What the runner gets when it claims a helper job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperClaim {
    pub job_id: i64,
    pub kind: HelperKind,
    pub app: crate::App,
    pub source: HelperSource,
    /// Claude Code session. Coordinator turns of one goal share a session.
    pub session_id: String,
    pub resume: bool,
    /// The full prompt, written by the hub.
    pub prompt: String,
    /// Tools Claude Code must not use in this job.
    pub disallowed_tools: Vec<String>,
    /// Set for pay-per-use apps; see [`crate::Claim::budget_usd`].
    #[serde(default)]
    pub budget_usd: Option<f64>,
    /// Pay-per-use only: the money set aside for this job, in micro-dollars.
    /// The gatekeeper stops the job's API calls once it is used.
    #[serde(default)]
    pub reserved_micros: Option<i64>,
    /// What earlier jobs of this Claude session were charged, in
    /// micro-dollars; cost reports are running totals that include it.
    #[serde(default)]
    pub prior_micros: i64,
    /// Files the owner attached for the app and this goal; see
    /// [`crate::Claim::context`].
    #[serde(default)]
    pub context: Vec<crate::context::ContextRef>,
}

/// How a helper job ended, posted by the runner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HelperResult {
    pub ok: bool,
    /// The agent's final answer, or what went wrong.
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// The same tokens split up, as in `RunEvent::Finished`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<crate::tokens::TokenUse>,
    /// `/out/page.html` from a design or review job, if the agent left one
    /// no bigger than [`MAX_PAGE_BYTES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_html: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply() -> CoordinatorReply {
        CoordinatorReply {
            say: "Here is the plan.".into(),
            questions: vec![],
            plan: Some(Plan {
                summary: "Add a reset link".into(),
                pieces: vec![PlanPiece {
                    title: "Reset link".into(),
                    brief: "Build it".into(),
                    done_when: "The link works".into(),
                    touches_protected: false,
                    research: false,
                }],
            }),
            remember: vec![],
            todo: vec![],
        }
    }

    #[test]
    fn only_a_plain_yes_counts_as_yes() {
        for yes in [
            "yes",
            "Yes, do it!",
            "  go ahead. ",
            "DO IT",
            "push",
            "ok",
            "lgtm",
            "👍",
            "yes - do it",
            "Ship it!!",
        ] {
            assert!(is_plain_yes(yes), "{yes:?} is a yes");
        }
        for not in [
            "",
            "no",
            "yes but make the button blue",
            "do it later",
            "not yet",
            "why?",
            "yes?",
            "go back",
            "don't do it",
            &"yes ".repeat(20),
        ] {
            assert!(!is_plain_yes(not), "{not:?} is not a plain yes");
        }
    }

    #[test]
    fn plans_and_questions_are_checked() {
        assert!(reply().validate().is_ok());
        let mut r = reply();
        r.questions.push(Question {
            id: "q1".into(),
            text: "Which?".into(),
            choices: vec!["a".into(), "b".into()],
            recommended: Some(0),
        });
        assert!(r.validate().is_err(), "questions and a plan together");
        r.plan = None;
        assert!(r.validate().is_ok());
        r.questions[0].recommended = Some(2);
        assert!(r.validate().is_err(), "recommends a missing choice");
        r.questions[0].recommended = None;
        r.questions[0].choices.truncate(1);
        assert!(r.validate().is_err(), "one choice is not a choice");
        let mut r = reply();
        if let Some(p) = r.plan.as_mut() {
            p.pieces.clear();
        }
        assert!(r.validate().is_err(), "empty plan");
    }

    #[test]
    fn finds_the_final_json_object() {
        let text = "Thinking about {braces} here.\n\n```json\n{\"pass\": true, \"summary\": \"Looks good\"}\n```\n";
        let v: ReviewVerdict = last_json_object(text).unwrap_or(ReviewVerdict {
            pass: false,
            summary: String::new(),
            issues: vec![],
            evidence: vec![],
            claims: vec![],
            questions: vec![],
        });
        assert!(v.pass);
        assert_eq!(v.summary, "Looks good");
        let two = r#"Example: {"say":"old"} and then {"say":"new"}"#;
        let r: Result<CoordinatorReply, _> = last_json_object(two);
        assert_eq!(r.map(|r| r.say).ok().as_deref(), Some("new"));
        let none: Result<ReviewVerdict, _> = last_json_object("no json");
        assert!(none.is_err());
        let trailing: Result<ReviewVerdict, _> =
            last_json_object(r#"{"pass":true,"summary":"x"} but then more words"#);
        assert!(trailing.is_err(), "the object must end the answer");
    }

    #[test]
    fn message_bodies_round_trip() {
        let b = GoalMessageBody::Coordinator(reply());
        let v = serde_json::to_value(&b).unwrap_or_default();
        assert_eq!(v["kind"], "coordinator");
        assert_eq!(v["plan"]["pieces"][0]["title"], "Reset link");
        let back: GoalMessageBody = serde_json::from_value(v).unwrap_or(GoalMessageBody::Note {
            text: String::new(),
        });
        assert_eq!(back, b);
        for s in GoalState::ALL {
            assert_eq!(s.as_str().parse::<GoalState>().ok(), Some(s));
        }
    }

    #[test]
    fn checker_receipts_round_trip_and_are_checked() {
        let text = r#"Done. {"pass": true, "summary": "Works.", "issues": [],
            "evidence": [{"ran": "the app's tests", "result": "42 of 42 passed"}],
            "claims": [{"text": "The form saves.", "how": "checked"},
                       {"text": "It should be fast enough.", "how": "guessed"}]}"#;
        let v: ReviewVerdict = last_json_object(text).expect("a verdict with receipts reads");
        assert_eq!(v.validate(), Ok(()));
        assert_eq!(v.evidence[0].result, "42 of 42 passed");
        assert_eq!(v.claims[0].how, Basis::Checked);
        assert_eq!(v.claims[1].how, Basis::Guessed);
        let json = serde_json::to_string(&v).expect("a verdict serializes");
        let back: ReviewVerdict = serde_json::from_str(&json).expect("and reads back");
        assert_eq!(back, v);

        // An older checker that leaves receipts out still reads, and the
        // empty lists are not written out.
        let old: ReviewVerdict =
            last_json_object(r#"{"pass": false, "summary": "Broken.", "issues": ["Fix it"]}"#)
                .expect("an old verdict reads");
        assert_eq!(old.validate(), Ok(()));
        assert!(old.evidence.is_empty() && old.claims.is_empty());
        let json = serde_json::to_string(&old).expect("serializes");
        assert!(
            !json.contains("evidence") && !json.contains("claims"),
            "{json}"
        );

        let mut many = v.clone();
        many.evidence = vec![v.evidence[0].clone(); MAX_EVIDENCE + 1];
        assert!(many.validate().is_err(), "11 evidence entries");
        let mut blank = v.clone();
        blank.evidence[0].ran = "  ".into();
        assert!(blank.validate().is_err(), "nothing said about what ran");
        let mut long = v.clone();
        long.claims[0].text = "x".repeat(501);
        assert!(long.validate().is_err(), "a claim over 500 characters");
        let unknown: Result<ReviewVerdict, _> = last_json_object(
            r#"{"pass": true, "summary": "x", "claims": [{"text": "y", "how": "felt"}]}"#,
        );
        assert!(unknown.is_err(), "only checked or guessed");
    }
}
