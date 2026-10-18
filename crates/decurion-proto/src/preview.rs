//! "Run it": the owner tries an app, or one piece of work, in a live browser
//! on the board or the phone.
//!
//! The owner taps Run it; the hub keeps a `preview` browser session (see
//! `crates/decurion-hub/src/browser.rs`) and the PC runner claims it with a
//! [`PreviewClaim`]. The runner starts the app's dev server in a sealed box
//! of its own, with no credential and the app's test keys by name, beside a
//! browser box behind the app's gate, and the owner drives that browser over
//! the same relay as the agent's test browser. Nothing is kept: the copy of
//! the code, the box and its home are thrown away when the preview stops.
//!
//! Only one preview runs at a time: a new one stops the one before. It stops
//! when the owner taps Stop, when nobody has had it open for
//! [`PREVIEW_IDLE_SECS`], or when the runner stops saying it is alive.

use crate::App;
use serde::{Deserialize, Serialize};

/// A preview nobody has had open for this long is stopped.
pub const PREVIEW_IDLE_SECS: u64 = 15 * 60;
/// How often the runner tells the hub a preview is still running, and asks
/// whether it should be.
pub const PREVIEW_ALIVE_SECS: u64 = 15;
/// A claimed preview whose runner has not said it is alive for this long is
/// taken to be gone (the runner stopped or the PC went to sleep).
pub const PREVIEW_STALE_SECS: u64 = 90;
/// A preview no runner has picked up for this long is closed: the PC's
/// runner is not on.
pub const PREVIEW_UNCLAIMED_SECS: u64 = 10 * 60;

/// A preview the PC runner picked up.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PreviewClaim {
    pub session_id: i64,
    pub app: App,
    /// The piece of work to run, as last saved. `None` runs the app as it
    /// is in its own folder on the PC.
    #[serde(default)]
    pub thread: Option<PreviewThread>,
    /// Names of the app's test keys. The runner reads their values from the
    /// vault on this PC; the hub never sends them.
    #[serde(default)]
    pub test_keys: Vec<String>,
}

/// Which saved copy of a piece of work a preview runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PreviewThread {
    pub thread_id: i64,
    pub title: String,
    pub branch: String,
    /// The latest saved copy (code and session) the hub holds.
    pub checkpoint: i64,
}

/// What the runner says every [`PREVIEW_ALIVE_SECS`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PreviewAlive {}

/// The hub's answer: whether the preview should keep running.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PreviewState {
    pub open: bool,
}

/// How a preview ended, when the runner ends it before its relay is up (or
/// without one): the owner's screen is told `why`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PreviewEnded {
    pub why: String,
    /// Something went wrong, rather than the preview simply being over.
    #[serde(default)]
    pub failed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Profile;

    #[test]
    fn a_claim_round_trips() {
        let c = PreviewClaim {
            session_id: 4,
            app: App {
                id: 1,
                name: "shop".into(),
                repo_path: "C:/apps/shop".into(),
                profile: Profile::A,
                pay_per_use: false,
                sites: vec![],
                monthly_cap_usd: None,
                reviewer: None,
                first_today: false,
            },
            thread: Some(PreviewThread {
                thread_id: 7,
                title: "New price list".into(),
                branch: "decurion/thread-7".into(),
                checkpoint: 3,
            }),
            test_keys: vec!["STRIPE_TEST_KEY".into()],
        };
        let s = serde_json::to_string(&c).unwrap_or_default();
        assert_eq!(serde_json::from_str::<PreviewClaim>(&s).ok(), Some(c));
        // An app preview has no thread.
        let v: PreviewClaim = serde_json::from_str(
            r#"{"session_id":5,"app":{"id":1,"name":"shop","repo_path":"x","profile":"A"}}"#,
        )
        .expect("app preview");
        assert!(v.thread.is_none() && v.test_keys.is_empty());
    }
}
