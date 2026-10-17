//! Types shared by the hub, the runner and the web board.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

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
    pub updated_at: String,
    /// The bigger task this one is part of. Stopping the parent stops this
    /// one first, unless it is `background`.
    #[serde(default)]
    pub parent_id: Option<i64>,
    /// Keeps going when its parent is stopped.
    #[serde(default)]
    pub background: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewThread {
    pub app_id: i64,
    pub title: String,
    pub prompt: String,
    #[serde(default)]
    pub parent_id: Option<i64>,
    #[serde(default)]
    pub background: bool,
    /// See [`check_receipt`].
    #[serde(default)]
    pub receipt: Option<String>,
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
/// running (the owner may have stopped it) and messages not yet delivered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Live {
    pub state: ThreadState,
    pub messages: Vec<Message>,
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
    /// Set for pay-per-use runs: the limit to give Claude Code. It is what
    /// earlier runs of this session cost plus most of the money set aside for
    /// this run, because Claude Code counts a resumed session's cost from the
    /// start.
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
}

/// A saved test key as the board sees it. The value itself is never shown
/// again after it is saved.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestKey {
    pub name: String,
    pub last_four: String,
    pub added_at: String,
}

/// A Max account that is out of usage, and the work waiting for it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Limit {
    pub profile: Profile,
    /// When the account is expected back (UTC). Waiting work starts again on
    /// its own after this; if the account is still out, the time moves on.
    pub until: String,
    pub waiting: i64,
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
