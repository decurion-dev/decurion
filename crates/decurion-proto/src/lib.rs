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
    /// Most this app's pay-per-use work may spend in a month, across every
    /// paid account. `None` means only the accounts' own caps apply.
    #[serde(default)]
    pub monthly_cap_usd: Option<f64>,
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
    #[serde(default)]
    pub ai: Ai,
    /// Claude Code session id, set on the first run so a restart resumes it.
    pub session_id: Option<String>,
    pub branch: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewThread {
    pub app_id: i64,
    pub title: String,
    pub prompt: String,
    /// Which AI does it. Defaults to Claude on the app's account.
    #[serde(default)]
    pub ai: Ai,
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
    /// Whose credential the run gets.
    pub account: Account,
    /// Set for pay-per-use runs: the limit to give Claude Code. It is what
    /// earlier runs of this session cost plus most of the money set aside for
    /// this run, because Claude Code counts a resumed session's cost from the
    /// start.
    #[serde(default)]
    pub budget_usd: Option<f64>,
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
