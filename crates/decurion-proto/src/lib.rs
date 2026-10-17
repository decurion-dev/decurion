//! Types shared by the hub, the runner and the web board.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// The version of the rules every runner must follow: how an agent's box is
/// built and how work is handed between computers. The hub refuses a runner
/// built with a different number, so a PC and a cloud worker can never run
/// the same work under different rules. Bump it whenever either changes.
pub const RULES_VERSION: u32 = 2;

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewApp {
    pub name: String,
    pub repo_path: String,
    pub profile: Profile,
    #[serde(default)]
    pub pay_per_use: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Thread {
    pub id: i64,
    pub app_id: i64,
    pub title: String,
    pub prompt: String,
    pub state: ThreadState,
    /// Claude Code session id, set on the first run so a restart resumes it.
    pub session_id: Option<String>,
    pub branch: Option<String>,
    /// Where the work runs. A runner only picks up work for its own place,
    /// and hands running work over when the owner moves it.
    #[serde(default)]
    pub place: Place,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewThread {
    pub app_id: i64,
    pub title: String,
    pub prompt: String,
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
}

/// What the runner gets when it claims queued work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub thread: Thread,
    pub app: App,
    pub resume: bool,
    /// Set for pay-per-use runs: the limit to give Claude Code. It is what
    /// earlier runs of this session cost plus most of the money set aside for
    /// this run, because Claude Code counts a resumed session's cost from the
    /// start.
    #[serde(default)]
    pub budget_usd: Option<f64>,
    /// The latest saved copy of this work (code and the agent's session) held
    /// by the hub. A runner whose own copy is not this one rebuilds from it,
    /// which is how work continues on another computer.
    #[serde(default)]
    pub checkpoint: Option<i64>,
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
}

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
}

/// Something that happened during a run, posted by the runner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunEvent {
    /// The runner started the agent's container. From here on, the run may
    /// be spending money.
    Started,
    /// The agent said something.
    Message { text: String },
    /// The agent used a tool.
    Tool { name: String, summary: String },
    /// The run ended. `ok` is the agent's own result, not a review. Cost and
    /// tokens are what Claude Code reported for this run.
    Finished {
        ok: bool,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_tokens: Option<u64>,
    },
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
        };
        let v = serde_json::to_value(&e).unwrap_or_default();
        assert_eq!(v["kind"], "finished");
        assert_eq!(v["ok"], true);
    }
}
