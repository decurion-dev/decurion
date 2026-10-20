//! Types shared by the hub, the runner and the web board.

pub mod addon;
pub mod browser;
pub mod context;
pub mod deploy;
pub mod goal;
pub mod plan;
pub mod preview;
pub mod sites;
pub mod tokens;

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// The version of the rules every runner must follow: how an agent's box is
/// built and how work is handed between computers. The hub refuses a runner
/// built with a different number, so a PC and a cloud worker can never run
/// the same work under different rules. Bump it whenever either changes.
pub const RULES_VERSION: u32 = 15;

/// What every agent that may run an app's tests (builder, research, checker)
/// is told about the private database in its box: `decurion-postgres` in the
/// claude-worker image. One wording for all of them, used in `concat!`.
#[macro_export]
macro_rules! local_database {
    () => {
        "If the app or its tests need PostgreSQL and your box has decurion-postgres (check with \
`command -v decurion-postgres`), start a private one inside your box, for example \
`decurion-postgres start --port 5433 --user postgres --db app --init db/init`. Copy the port, \
POSTGRES_USER, POSTGRES_DB and the folder mounted at /docker-entrypoint-initdb.d from the app's \
docker-compose file (or its .env example); it sets the database up the way that postgres image would. \
It listens only inside your box and accepts any password. Its data is kept for this piece of work; to \
set it up afresh, run `decurion-postgres reset --port 5433` and start it again. Run the app's \
database tests against it instead of skipping them. "
    };
}

/// Where a piece of work is. A thread is in exactly one state at a time, and
/// only the moves in [`ThreadState::can_move_to`] are allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadState {
    Queued,
    Running,
    NeedsYou,
    InReview,
    Done,
    Failed,
}

impl ThreadState {
    pub const ALL: [ThreadState; 6] = [
        Self::Queued,
        Self::Running,
        Self::NeedsYou,
        Self::InReview,
        Self::Done,
        Self::Failed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::NeedsYou => "needs_you",
            Self::InReview => "in_review",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    /// The allowed moves. Going back to `Queued` hands work to the runner
    /// again: from `Running` it is a pause (runner crashed, PC asleep); from
    /// `NeedsYou` or `InReview` the owner is sending it back to the agent.
    pub fn can_move_to(self, to: ThreadState) -> bool {
        use ThreadState::*;
        matches!(
            (self, to),
            (Queued, Running)
                | (Queued, NeedsYou)
                | (Queued, Failed)
                | (Running, Queued)
                | (Running, NeedsYou)
                | (Running, InReview)
                | (Running, Done)
                | (Running, Failed)
                | (NeedsYou, Queued)
                | (NeedsYou, Failed)
                | (InReview, Queued)
                | (InReview, Done)
                | (InReview, Failed)
                | (Failed, Queued)
        )
    }
}

impl fmt::Display for ThreadState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown thread state: {0}")]
pub struct UnknownState(pub String);

impl FromStr for ThreadState {
    type Err = UnknownState;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|st| st.as_str() == s)
            .ok_or_else(|| UnknownState(s.to_owned()))
    }
}

/// Which Claude Max account an app runs on. Never switched automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Profile {
    A,
    B,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
        }
    }
}

impl FromStr for Profile {
    type Err = UnknownState;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "A" => Ok(Self::A),
            "B" => Ok(Self::B),
            other => Err(UnknownState(other.to_owned())),
        }
    }
}

/// Which AI does a piece of work. Claude runs on the app's Max account (or
/// pay-per-use); ChatGPT and Grok run through pi on their own API keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ai {
    #[default]
    Claude,
    Chatgpt,
    Grok,
}

impl Ai {
    pub const ALL: [Ai; 3] = [Self::Claude, Self::Chatgpt, Self::Grok];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Chatgpt => "chatgpt",
            Self::Grok => "grok",
        }
    }

    /// Claude Code reports a resumed session's cost from the session's start,
    /// so its limit and its cost report include earlier runs. pi reports only
    /// what the run itself cost.
    pub fn reports_session_cost(self) -> bool {
        matches!(self, Self::Claude)
    }
}

impl FromStr for Ai {
    type Err = UnknownState;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|a| a.as_str() == s)
            .ok_or_else(|| UnknownState(s.to_owned()))
    }
}

/// Who pays for a run. The two Max plans are flat-rate; the rest are billed
/// per use and each has its own monthly cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Account {
    /// Claude Max plan A.
    A,
    /// Claude Max plan B.
    B,
    /// The pay-per-use Anthropic API key.
    #[serde(rename = "api")]
    Api,
    /// The OpenAI API key, for ChatGPT work.
    #[serde(rename = "openai")]
    OpenAi,
    /// The xAI API key, for Grok work.
    #[serde(rename = "xai")]
    Xai,
}

impl Account {
    pub const ALL: [Account; 5] = [Self::A, Self::B, Self::Api, Self::OpenAi, Self::Xai];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::Api => "api",
            Self::OpenAi => "openai",
            Self::Xai => "xai",
        }
    }

    /// What the owner sees on cards and meters.
    pub fn label(self) -> &'static str {
        match self {
            Self::A => "Claude A",
            Self::B => "Claude B",
            Self::Api => "Claude pay-per-use",
            Self::OpenAi => "ChatGPT",
            Self::Xai => "Grok",
        }
    }

    /// Billed per use, under a monthly cap.
    pub fn is_paid(self) -> bool {
        !matches!(self, Self::A | Self::B)
    }

    /// The account a piece of work runs on.
    pub fn for_work(ai: Ai, app: &App) -> Account {
        match ai {
            Ai::Claude if app.pay_per_use => Self::Api,
            Ai::Claude => match app.profile {
                Profile::A => Self::A,
                Profile::B => Self::B,
            },
            Ai::Chatgpt => Self::OpenAi,
            Ai::Grok => Self::Xai,
        }
    }
}

impl FromStr for Account {
    type Err = UnknownState;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|a| a.as_str() == s)
            .ok_or_else(|| UnknownState(s.to_owned()))
    }
}

/// Which computer a piece of work runs on. The owner moves work between them
/// with one menu choice; the rules are the same on both.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Place {
    #[default]
    Pc,
    Cloud,
}

impl Place {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pc => "pc",
            Self::Cloud => "cloud",
        }
    }
}

impl FromStr for Place {
    type Err = UnknownState;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pc" => Ok(Self::Pc),
            "cloud" => Ok(Self::Cloud),
            other => Err(UnknownState(other.to_owned())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct App {
    pub id: i64,
    pub name: String,
    /// Local path of the app's git checkout. Worktrees are cut from it.
    pub repo_path: String,
    pub profile: Profile,
    /// Runs on the pay-per-use API key, under the monthly cap, instead of
    /// the Max account in `profile`.
    #[serde(default)]
    pub pay_per_use: bool,
    /// Websites this app's agents may reach on top of [`sites::BUILT_IN`].
    #[serde(default)]
    pub sites: Vec<String>,
    /// Most this app's pay-per-use work may spend in a month, across every
    /// paid account. `None` means only the accounts' own caps apply.
    #[serde(default)]
    pub monthly_cap_usd: Option<f64>,
    /// The GitHub login who reviews this app's pull requests. Each one asks
    /// them for a review and says they review next.
    #[serde(default)]
    pub reviewer: Option<String>,
    /// The owner starred this app to go first in the queue today. Ends by
    /// itself when the day does.
    #[serde(default)]
    pub first_today: bool,
    /// What Decurion may do for this app, as the owner set it on the app's
    /// own Settings page.
    #[serde(default)]
    pub rules: AppRules,
}

/// The owner's rules for one app. Only the owner sets them (owner-only
/// route, never a runner, agent or file in the repo). Merging and deploying
/// still take one owner tap every time (PLAN.md D7): these only say whether
/// Decurion shows that tap for this app. Every app starts at "PR only".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AppRules {
    /// The owner may merge this app's pull requests from Decurion, one tap
    /// each. Off: people merge on GitHub.
    pub merge: bool,
    /// The owner may deploy this app from Decurion, one tap each, running
    /// [`AppRules::deploy_steps`].
    pub deploy: bool,
    /// The owner's deploy steps as typed, one command per line
    /// ([`deploy::parse_steps`]). Empty when none are saved.
    pub deploy_steps: String,
    /// Where Open site goes once a deploy finishes ([`deploy::check_site`]).
    pub deploy_site: Option<String>,
    /// A goal's piece that passes its check goes to GitHub by itself. Off:
    /// it waits in review for the owner's Accept.
    pub send_checked: bool,
    /// The app's live version holds patient or personal data, so agents are
    /// told to use made-up data only. Decurion itself never handles it.
    pub patient_data: bool,
    /// When one of this app's pull requests conflicts with its base branch,
    /// the pull request watcher starts a Bring up to date run by itself,
    /// once per pushed head and base tip. Only the start is automatic: the
    /// result waits in review for the owner's Accept like any other work.
    /// Off for every app until the owner turns it on.
    pub update_conflicts: bool,
}

impl Default for AppRules {
    /// "PR only": Decurion pushes and opens pull requests, nothing more.
    fn default() -> Self {
        Self {
            merge: false,
            deploy: false,
            deploy_steps: String::new(),
            deploy_site: None,
            send_checked: true,
            patient_data: true,
            update_conflicts: false,
        }
    }
}

impl AppRules {
    /// The steps one Deploy tap runs: only when deploying is on and the
    /// saved steps read cleanly. `None` means there is no Deploy button and
    /// the hub refuses a deploy, whatever the request says.
    pub fn deploy_steps(&self) -> Option<Vec<deploy::Step>> {
        if !self.deploy {
            return None;
        }
        deploy::parse_steps(&self.deploy_steps).ok()
    }

    /// "PR only" (merge and deploy off), "Full control" (both on), or a mix.
    pub fn level(&self) -> RulesLevel {
        match (self.merge, self.deploy) {
            (false, false) => RulesLevel::PrOnly,
            (true, true) => RulesLevel::Full,
            _ => RulesLevel::Mix,
        }
    }
}

/// How far an app's rules go, for a one-word label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesLevel {
    PrOnly,
    Full,
    Mix,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewApp {
    pub name: String,
    pub repo_path: String,
    pub profile: Profile,
    #[serde(default)]
    pub pay_per_use: bool,
    #[serde(default)]
    pub sites: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Thread {
    pub id: i64,
    pub app_id: i64,
    pub title: String,
    pub prompt: String,
    pub state: ThreadState,
    #[serde(default)]
    pub ai: Ai,
    /// The agent's session id, set on the first run so a restart resumes it.
    pub session_id: Option<String>,
    pub branch: Option<String>,
    /// Where the work runs. A runner only picks up work for its own place,
    /// and hands running work over when the owner moves it.
    #[serde(default)]
    pub place: Place,
    pub updated_at: String,
    /// The bigger task this one is part of. Stopping the parent stops this
    /// one first, unless it is `background`.
    #[serde(default)]
    pub parent_id: Option<i64>,
    /// Keeps going when its parent is stopped.
    #[serde(default)]
    pub background: bool,
    /// Accepted work sent to GitHub by the hub, once the owner accepts it.
    #[serde(default)]
    pub github: Option<GitHubSend>,
    /// This work fixes review comments on the pull request that work
    /// opened, and goes to that same branch when accepted.
    #[serde(default)]
    pub fix_of: Option<i64>,
    /// A research piece of a goal: its agent studies the app and writes a
    /// report instead of changing code. It goes to review without commits,
    /// skips the checker, and is never sent to GitHub.
    #[serde(default)]
    pub research: bool,
    /// When the owner dismissed this finished work. It is then shown only
    /// in the Dismissed view; nothing about the work itself changes.
    #[serde(default)]
    pub dismissed_at: Option<String>,
    /// Why this work, which has no goal piece, waits for the owner to look
    /// at it before Accept (it changes protected parts of the app). Set by
    /// the hub when a Bring up to date run reaches review.
    #[serde(default)]
    pub hold: Option<String>,
    /// For finished work that went to GitHub: where it is on its way from
    /// pushed to merged (and live), worked out by the hub from the app's
    /// rules, the same way the goal page tells it. Only the hub fills it,
    /// and only for screens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub road: Option<goal::Road>,
    /// Decurion started this work by itself, under the app's rule to fix
    /// conflicts with main ([`AppRules::update_conflicts`]), not on the
    /// owner's tap. It still waits for the owner's Accept.
    #[serde(default)]
    pub auto_started: bool,
}

/// Where sending accepted work to GitHub has got to. Only the hub sends,
/// never a box, and it never merges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendState {
    /// Accepted; the hub will send it.
    Waiting,
    /// The hub is sending it now.
    Sending,
    /// On GitHub (or the app's own git server) as a branch.
    Sent,
    /// Not sent; `error` says why and the owner can try again.
    Failed,
}

impl SendState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::Failed => "failed",
        }
    }
}

impl FromStr for SendState {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "waiting" => Self::Waiting,
            "sending" => Self::Sending,
            "sent" => Self::Sent,
            "failed" => Self::Failed,
            other => return Err(format!("unknown send state {other:?}")),
        })
    }
}

/// What the board shows about accepted work sent to GitHub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubSend {
    pub state: SendState,
    /// The branch the work goes to, always under `decurion/`.
    pub branch: String,
    /// The pull request, or a link that opens one, once sent to GitHub.
    #[serde(default)]
    pub url: Option<String>,
    /// True when `url` is a pull request Decurion opened; false when it is
    /// a link for the owner to open one.
    #[serde(default)]
    pub pull_request: bool,
    /// Plain words for the owner: why it was not sent, or why no pull
    /// request was opened.
    #[serde(default)]
    pub error: Option<String>,
    /// GitHub says the pull request clashes with newer changes on its base
    /// branch, so it can't be merged as it is.
    #[serde(default)]
    pub clash: bool,
    /// Its pull request was merged on GitHub.
    #[serde(default)]
    pub merged: bool,
    /// Its pull request was closed on GitHub without merging.
    #[serde(default)]
    pub closed: bool,
    /// Its merged change is live: in a release from Decurion, or, for
    /// Decurion's own code, in the Decurion running on the owner's PC.
    #[serde(default)]
    pub live: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewThread {
    pub app_id: i64,
    pub title: String,
    pub prompt: String,
    /// Which AI does it. Defaults to Claude on the app's account.
    #[serde(default)]
    pub ai: Ai,
    #[serde(default)]
    pub parent_id: Option<i64>,
    #[serde(default)]
    pub background: bool,
    /// See [`check_receipt`].
    #[serde(default)]
    pub receipt: Option<String>,
    /// Where the work runs (the PC unless a cloud worker exists).
    #[serde(default)]
    pub place: Place,
}

/// The owner's "Move to PC / Move to cloud" choice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceRequest {
    pub place: Place,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveRequest {
    pub to: ThreadState,
    #[serde(default)]
    pub note: Option<String>,
    /// The state the caller saw. Set, the move only happens if the thread is
    /// still in it, so a tap on a screen that has gone stale (Stop on work
    /// that has since finished) is refused instead of doing something else.
    #[serde(default)]
    pub expect: Option<ThreadState>,
    /// See [`check_receipt`].
    #[serde(default)]
    pub receipt: Option<String>,
}

/// A receipt number is chosen by the browser or phone for each tap and sent
/// again unchanged on a retry. The hub acts on a receipt once; a resend gets
/// the first answer back instead of acting twice. 8 to 64 letters, digits,
/// `-` or `_` (a UUID fits).
pub fn check_receipt(r: &str) -> bool {
    (8..=64).contains(&r.len())
        && r.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Whether a Bring up to date run merged main the way it was asked to,
/// read from `git rev-list --parents <head> --not <start> <merge_from>`:
/// the commits it added, each with its parents. No commit may have more
/// than two parents (no octopus merge), and at least one of them must be a
/// merge with `merge_from` as a parent. The runner checks this when the run
/// ends and the hub again before sending, along with the head descending
/// from both the start and `merge_from`.
pub fn merged_as_asked(rev_list_parents: &str, merge_from: &str) -> bool {
    let mut merged = false;
    for line in rev_list_parents.lines().filter(|l| !l.trim().is_empty()) {
        let parents: Vec<&str> = line.split_whitespace().skip(1).collect();
        if parents.len() > 2 {
            return false;
        }
        merged |= parents.len() == 2 && parents.contains(&merge_from);
    }
    merged
}

/// How a message from the owner reaches an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// Change course now: the agent's current step is stopped and it gets
    /// the message straight away.
    Steer,
    /// Do this next: the agent gets it when its current job is done.
    FollowUp,
    /// Extra information: the agent sees it at its next step without being
    /// stopped.
    Note,
}

impl MessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::FollowUp => "follow_up",
            Self::Note => "note",
        }
    }
}

impl FromStr for MessageKind {
    type Err = UnknownState;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        [Self::Steer, Self::FollowUp, Self::Note]
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| UnknownState(s.to_owned()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewMessage {
    pub kind: MessageKind,
    pub text: String,
    /// See [`check_receipt`].
    #[serde(default)]
    pub receipt: Option<String>,
    /// Set by the owner's screen when it shows the work's Accept and no
    /// question from the agent: a plain yes ([`goal::is_plain_yes`]) is then
    /// that Accept. Otherwise "yes" may be an answer to the agent, and is
    /// sent as one.
    #[serde(default)]
    pub accept_on_yes: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: i64,
    pub thread_id: i64,
    pub kind: MessageKind,
    pub text: String,
    pub at: String,
    /// When the runner handed it to the agent. Unset while it waits.
    pub delivered_at: Option<String>,
}

/// What the runner checks while a run is going: whether the thread is still
/// running (the owner may have stopped it), which computer it belongs on
/// (the owner may have moved it) and messages not yet delivered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Live {
    pub state: ThreadState,
    #[serde(default)]
    pub place: Place,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub browser: Option<crate::browser::BrowserWant>,
}

/// The step an agent was in the middle of when its run was cut off.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Interrupted {
    pub name: String,
    pub summary: String,
    /// See [`safe_to_redo`].
    pub safe_to_redo: bool,
}

/// Whether a Claude Code tool only looks at things, so running it again
/// after a crash changes nothing. Anything else (running commands, editing
/// files, unknown tools) may already have happened and is never simply
/// repeated: the agent is told to check first.
pub fn safe_to_redo(tool: &str) -> bool {
    matches!(
        tool,
        "Read" | "Glob" | "Grep" | "LS" | "WebFetch" | "WebSearch" | "TodoWrite" | "BashOutput"
    )
}

/// What the runner gets when it claims queued work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub thread: Thread,
    pub app: App,
    pub resume: bool,
    /// Whose credential the run gets.
    pub account: Account,
    /// Set for pay-per-use runs: the run's spending limit, most of the money
    /// set aside for it. For Claude Code it also includes what earlier runs of
    /// this session cost, because Claude Code counts a resumed session's cost
    /// from the start (see [`Ai::reports_session_cost`]). pi has no limit of
    /// its own; the runner stops it here.
    #[serde(default)]
    pub budget_usd: Option<f64>,
    /// Owner messages not yet delivered, oldest first. The runner gives them
    /// to the agent with its starting prompt.
    #[serde(default)]
    pub messages: Vec<Message>,
    /// Set when a resumed run was cut off in the middle of a step.
    #[serde(default)]
    pub interrupted: Option<Interrupted>,
    /// Names of the app's test keys. The runner reads their values from the
    /// vault on this PC; the hub never sends them.
    #[serde(default)]
    pub test_keys: Vec<String>,
    /// Set for pay-per-use runs: the money set aside for this run alone, in
    /// micro-dollars. The gatekeeper stops the run's API calls as it is used.
    #[serde(default)]
    pub reserved_micros: Option<i64>,
    /// What earlier runs of this Claude session were charged, in
    /// micro-dollars. Cost reports are running totals for the session, so
    /// they include this.
    #[serde(default)]
    pub prior_micros: i64,
    /// The latest saved copy of this work (code and the agent's session) held
    /// by the hub. A runner whose own copy is not this one rebuilds from it,
    /// which is how work continues on another computer.
    #[serde(default)]
    pub checkpoint: Option<i64>,
    /// The app's saved website sign-ins that still work. The runner opens
    /// them from the vault on this PC; the hub never holds them.
    #[serde(default)]
    pub sign_ins: Vec<SignInRef>,
    /// Files the owner attached for this work: the app's, and its goal's.
    /// The runner fetches them from the hub and copies them into the box at
    /// [`context::BOX_DIR`] before the agent starts.
    #[serde(default)]
    pub context: Vec<context::ContextRef>,
    /// Add-ons the owner turned on for this app. The runner fetches each
    /// one's files from the hub, checks their hash and starts its tools in
    /// boxes of their own beside the agent's (see [`addon`]).
    #[serde(default)]
    pub addons: Vec<addon::AddonUse>,
    /// For a Bring up to date run: the commit of the pull request's base
    /// branch to merge in. The starting bundle then also holds it, as
    /// `refs/heads/decurion-main`, and the runner makes the local branch
    /// `decurion-main` at exactly this commit before the agent starts.
    #[serde(default)]
    pub merge_from: Option<String>,
}

/// A saved website sign-in named in a claim: which one to open from the
/// vault, and what the run needs to know about it. Never the sign-in itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SignInRef {
    pub id: i64,
    /// The owner's name for it, such as "Staging admin".
    pub label: String,
    /// A page that needs the sign-in, checked before the agent starts.
    #[serde(default)]
    pub check_url: Option<String>,
    /// Where the site sends someone who is not signed in.
    #[serde(default)]
    pub login_url: Option<String>,
    /// Which saved copy in the vault is current.
    pub version: i64,
}

/// A sign-in window the PC runner picked up: the owner tapped "Sign in" and
/// a fresh browser should open at `start_url`. What the owner signs in to is
/// sealed in the vault as `version` of sign-in `sign_in_id` of `app_id`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SignInSession {
    pub session_id: i64,
    pub sign_in_id: i64,
    pub app_id: i64,
    pub start_url: String,
    pub version: i64,
}

/// What the runner reports once a sign-in is sealed in the vault.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SignInSaved {
    pub session_id: i64,
    /// The sites it covers, as the browser saw them.
    #[serde(default)]
    pub domains: Vec<String>,
    /// When its first lasting cookie ends, in Unix seconds.
    #[serde(default)]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub login_url: Option<String>,
    /// The version it was sealed as: the one in its [`SignInSession`].
    pub version: i64,
}

/// A saved test key as the board sees it. The value itself is never shown
/// again after it is saved.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestKey {
    pub name: String,
    pub last_four: String,
    pub added_at: String,
}

/// What a runner says when it starts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerHello {
    pub runner: String,
    pub rules_version: u32,
}

/// The hub's answer to [`RunnerHello`]. Sent with 200 when the runner may
/// work, and with 409 when its rules version differs, so it can update itself
/// to `hub_commit` and start again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloReply {
    /// Work this runner had in flight when it stopped, now queued to resume.
    #[serde(default)]
    pub paused: Vec<i64>,
    pub rules_version: u32,
    /// The git commit the hub was built from, or "unknown".
    pub hub_commit: String,
    /// The hub honours a claim's `busy` list and a sign-in report's thread
    /// and version, so a runner may work on several pieces at once. An
    /// older hub leaves it out, and the runner then works on one at a time.
    #[serde(default)]
    pub concurrent: bool,
}

/// Most pieces of work one runner may list as busy in a claim, and so the
/// most it may run at once.
pub const MAX_RUNS: usize = 8;

/// A saved copy the runner finished uploading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewCheckpoint {
    /// The branch head inside the uploaded code bundle.
    pub head: String,
    /// The starting commit the runner's copy was made from. Work started
    /// before Phase 2 has no starting commit on the hub yet; this sets it, so
    /// another computer rebuilds the copy from the same place.
    #[serde(default)]
    pub base: Option<String>,
}

/// The credential a run needs, handed to a runner that has no local copy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCredential {
    /// The environment variable Claude Code reads it from.
    pub var: String,
    pub value: String,
}

/// A cloud worker's sign-up: the one-time code from the setup page, swapped
/// for its own long-lived key on first start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinReply {
    pub name: String,
    pub token: String,
}

/// A Max account that is out of usage, and the work waiting for it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Limit {
    pub profile: Profile,
    /// When the account is expected back (UTC). Waiting work starts again on
    /// its own after this; if the account is still out, the time moves on.
    pub until: String,
    /// Work waiting for the reset. Work that runs on pay-per-use meanwhile
    /// does not count.
    pub waiting: i64,
    /// Set after the owner's "Continue on pay-per-use" tap: until this time
    /// (UTC, the reset time) the account's Claude work runs on pay-per-use,
    /// within the monthly cap, instead of waiting. Then it goes back to Max.
    #[serde(default)]
    pub pay_per_use_until: Option<String>,
}

/// This month's spending, for the board.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Spend {
    /// Pay-per-use spending this month, counting money set aside for runs
    /// still going.
    pub pay_per_use_usd: f64,
    pub cap_usd: f64,
    /// What this month's Max-account runs would have cost on pay-per-use.
    /// Not billed; shown so usage is visible.
    pub max_equivalent_usd: f64,
    /// This month's pay-per-use spending per app, with each app's own cap.
    #[serde(default)]
    pub apps: Vec<AppSpend>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppSpend {
    pub app_id: i64,
    pub spent_usd: f64,
    pub cap_usd: Option<f64>,
}

/// One account's row on the accounts panel and the top-bar meter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountMeter {
    pub account: Account,
    pub label: String,
    /// Paid accounts: money spent this month, counting money set aside for
    /// runs still going.
    #[serde(default)]
    pub spent_usd: Option<f64>,
    /// Paid accounts: the monthly cap.
    #[serde(default)]
    pub cap_usd: Option<f64>,
    /// Max plans: share of the weekly allowance used, 0 to 100, as Claude
    /// last reported it.
    #[serde(default)]
    pub week_pct: Option<f64>,
    /// Max plans: share of the current five-hour allowance used.
    #[serde(default)]
    pub five_hour_pct: Option<f64>,
    /// Max plans: when the limit that is closest to running out resets.
    #[serde(default)]
    pub resets_at: Option<String>,
    /// Max plans: when the weekly allowance shown in `week_pct` renews.
    #[serde(default)]
    pub week_resets_at: Option<String>,
    /// Max plans: when the current five-hour allowance renews.
    #[serde(default)]
    pub five_hour_resets_at: Option<String>,
    /// Max plans: the last report said this plan is out for now.
    #[serde(default)]
    pub limited: bool,
    /// When the Max figures were last reported; `None` if never.
    #[serde(default)]
    pub seen_at: Option<String>,
}

/// Something that happened during a run, posted by the runner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunEvent {
    /// The runner started the agent's container. From here on, the run may
    /// be spending money.
    Started,
    /// The Max account this run used is out of usage. `reset_epoch` is when
    /// Claude said it resets (Unix seconds), if it said.
    LimitHit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reset_epoch: Option<i64>,
    },
    /// The agent said something.
    Message { text: String },
    /// The agent used a tool.
    Tool { name: String, summary: String },
    /// The box tried to reach a website that is not allowed, and the gate
    /// turned it away. `reason` is in plain words.
    Blocked { host: String, reason: String },
    /// The run ended. `ok` is the agent's own result, not a review. Cost and
    /// tokens are what the agent reported: for Claude Code, the whole
    /// session so far; for pi, this run.
    Finished {
        ok: bool,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_tokens: Option<u64>,
        /// The same tokens split into fresh input, cache reads and cache
        /// writes, with how full the conversation got. For Claude Code, the
        /// counts are the whole session so far (its `modelUsage`), like the
        /// cost; the peak and compactions are this run's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens: Option<tokens::TokenUse>,
    },
    /// Claude reported how much of a Max plan's allowance is used. The hub
    /// keeps the latest reading per plan and window for the meters; it is
    /// not part of the activity log. Every field is optional so a reading
    /// Claude Code shapes differently in future still gets through.
    Usage {
        /// Which allowance, as Claude names it: `five_hour`, `seven_day`, ...
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<String>,
        /// Share of that allowance used, 0 to 100.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        used_pct: Option<f64>,
        /// When that allowance renews, in Unix seconds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resets_at: Option<i64>,
        /// The plan as a whole: `allowed`, `allowed_warning` or `rejected`
        /// (out for now). Only set on the reading for the allowance Claude is
        /// reporting the status of.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
    },
    /// Written by the hub, never the runner: the owner sent the agent a
    /// message.
    Owner { via: MessageKind, text: String },
    /// Written by the hub, never the runner: the run was cut off while the
    /// agent was in the middle of this step.
    Interrupted {
        name: String,
        summary: String,
        safe_to_redo: bool,
    },
}

impl RunEvent {
    /// Kinds only the hub writes. The runner API refuses them.
    pub fn hub_only(&self) -> bool {
        matches!(self, Self::Owner { .. } | Self::Interrupted { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub id: i64,
    pub thread_id: i64,
    pub at: String,
    pub event: RunEvent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_must_merge_main_with_one_plain_merge() {
        let (m, x, y, a) = (
            "m".repeat(40),
            "x".repeat(40),
            "y".repeat(40),
            "a".repeat(40),
        );
        // A fix after the merge, then the merge of main into the branch.
        assert!(merged_as_asked(&format!("{a} {x}\n{x} {y} {m}\n"), &m));
        // No merge at all, or one that is not of main.
        assert!(!merged_as_asked(&format!("{x} {y}\n"), &m));
        assert!(!merged_as_asked(&format!("{x} {y} {a}\n"), &m));
        assert!(!merged_as_asked("", &m));
        // An octopus merge, even one that includes main.
        assert!(!merged_as_asked(&format!("{x} {y} {m} {a}\n"), &m));
    }

    #[test]
    fn terminal_states_cannot_move_except_retry() {
        for to in ThreadState::ALL {
            assert!(!ThreadState::Done.can_move_to(to), "done -> {to}");
            assert_eq!(
                ThreadState::Failed.can_move_to(to),
                to == ThreadState::Queued,
                "failed -> {to}"
            );
        }
    }

    #[test]
    fn queued_cannot_skip_to_done() {
        assert!(!ThreadState::Queued.can_move_to(ThreadState::Done));
        assert!(!ThreadState::Queued.can_move_to(ThreadState::InReview));
    }

    #[test]
    fn states_round_trip_through_strings_and_json() {
        for st in ThreadState::ALL {
            assert_eq!(st.as_str().parse::<ThreadState>().ok(), Some(st));
            let json = serde_json::to_string(&st).unwrap_or_default();
            assert_eq!(json, format!("\"{}\"", st.as_str()));
        }
        assert!("paused".parse::<ThreadState>().is_err());
    }

    #[test]
    fn receipts_and_message_kinds() {
        assert!(check_receipt("0b7e6c1a-6f43-4c0e-9d0a-2f1f1d3c9e11"));
        assert!(!check_receipt("short"));
        assert!(!check_receipt("has space in it"));
        assert!(!check_receipt(&"x".repeat(65)));
        for k in [MessageKind::Steer, MessageKind::FollowUp, MessageKind::Note] {
            assert_eq!(k.as_str().parse::<MessageKind>().ok(), Some(k));
            let json = serde_json::to_string(&k).unwrap_or_default();
            assert_eq!(json, format!("\"{}\"", k.as_str()));
        }
        assert!(safe_to_redo("Read") && !safe_to_redo("Bash") && !safe_to_redo("Whatever"));
        assert!(RunEvent::Owner {
            via: MessageKind::Note,
            text: "x".into()
        }
        .hub_only());
        assert!(!RunEvent::Started.hub_only());
    }

    #[test]
    fn place_defaults_to_pc_and_round_trips() {
        let t: NewThread =
            serde_json::from_str(r#"{"app_id":1,"title":"t","prompt":"p"}"#).expect("json");
        assert_eq!(t.place, Place::Pc);
        for p in [Place::Pc, Place::Cloud] {
            assert_eq!(p.as_str().parse::<Place>().ok(), Some(p));
            assert_eq!(
                serde_json::to_string(&p).unwrap_or_default(),
                format!("\"{}\"", p.as_str())
            );
        }
    }

    #[test]
    fn run_event_json_shape() {
        let e = RunEvent::Finished {
            ok: true,
            summary: "done".into(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            tokens: None,
        };
        let v = serde_json::to_value(&e).unwrap_or_default();
        assert_eq!(v["kind"], "finished");
        assert_eq!(v["ok"], true);
    }

    #[test]
    fn usage_events_tolerate_missing_fields() {
        let e: Option<RunEvent> = serde_json::from_str(r#"{"kind":"usage"}"#).ok();
        assert_eq!(
            e,
            Some(RunEvent::Usage {
                window: None,
                used_pct: None,
                resets_at: None,
                status: None
            })
        );
        let e = RunEvent::Usage {
            window: Some("seven_day".into()),
            used_pct: Some(42.0),
            resets_at: Some(1_760_000_000),
            status: None,
        };
        let v = serde_json::to_value(&e).unwrap_or_default();
        assert_eq!(v["kind"], "usage");
        assert_eq!(v["used_pct"], 42.0);
        assert!(v.get("status").is_none());
    }
}
