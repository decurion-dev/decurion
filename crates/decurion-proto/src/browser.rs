//! The agent's test browser, run in a box of its own beside the agent's box
//! (see `docker/claude-worker/browser-host.mjs`), and what flows between it,
//! the runner, the hub and the owner's screen.
//!
//! The browser box and the runner talk over the box's stdin and stdout, one
//! JSON object per line ([`ToBrowser`], [`FromBrowser`]). Saved sign-ins only
//! ever go in on stdin: never in a file, a variable or an argument. The
//! runner relays screen frames to the hub, and the owner's input back, over a
//! WebSocket it opens itself ([`RelayUp`], [`RelayDown`]); the owner's screen
//! talks to the hub with [`ViewerUp`] and [`ViewerDown`].

use serde::{Deserialize, Serialize};

/// The most a screen frame may weigh on the wire, base64 included.
pub const MAX_FRAME_BYTES: usize = 512 << 10;
/// The most one piece of input may weigh: typed text is sent in pieces.
pub const MAX_INPUT_BYTES: usize = 4 << 10;
/// The most a saved sign-in may weigh.
pub const MAX_STATE_BYTES: usize = 2 << 20;
/// The owner's control ends on its own after this long without input.
pub const IDLE_HAND_BACK_SECS: u64 = 10 * 60;

/// What the browser box is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserMode {
    /// The agent's test browser for one run, with the app's saved sign-ins.
    Run,
    /// A fresh browser the owner signs in with, to save that sign-in.
    SignIn,
    /// The owner trying an app: a browser behind the app's gate, beside a box
    /// running the app's dev server, which it opens once the app answers.
    Preview,
}

/// Who is driving the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Controller {
    #[default]
    Agent,
    Owner,
}

/// A saved sign-in handed to the browser box: Playwright's `storageState`
/// (cookies and each origin's localStorage), opened only on the runner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteState {
    pub sign_in_id: i64,
    pub state: serde_json::Value,
    /// A page that needs the sign-in, checked before the agent starts.
    #[serde(default)]
    pub check_url: Option<String>,
    /// Where the site sends someone who is not signed in.
    #[serde(default)]
    pub login_url: Option<String>,
}

/// One piece of the owner's input. Positions are fractions (0 to 1) of the
/// frame, so the screen size does not matter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Input {
    Click {
        x: f64,
        y: f64,
    },
    /// Scrolls at `x`, `y`. `dx` and `dy` are how far, also as fractions of
    /// the frame; a positive `dy` scrolls down, as a mouse wheel does.
    Scroll {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    },
    /// Text typed into whatever has focus.
    Text {
        text: String,
    },
    /// One of a few named keys: Enter, Tab, Backspace, Escape, ArrowUp,
    /// ArrowDown, ArrowLeft, ArrowRight.
    Key {
        key: String,
    },
    /// Go to an address (http or https only).
    Go {
        url: String,
    },
    Back,
}

/// Runner to browser box, one per line on its stdin. `Start` comes first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToBrowser {
    Start {
        mode: BrowserMode,
        #[serde(default)]
        states: Vec<SiteState>,
        /// Sign-in mode: where the window opens.
        #[serde(default)]
        start_url: Option<String>,
    },
    /// Send frames (someone is watching) or stop.
    Screencast {
        on: bool,
        max_width: u32,
        quality: u8,
    },
    /// Acknowledges a frame, so the next one may come.
    FrameAck {
        seq: u64,
    },
    Input {
        input: Input,
    },
    Control {
        who: Controller,
    },
    /// Sign-in mode: save what the owner signed in to.
    Capture,
    /// Preview mode: a line of progress for the waiting page ("Installing
    /// what the app needs"). Shown as text, never as HTML.
    Note {
        text: String,
    },
    /// Preview mode: open this address (on the box's own localhost) once it
    /// answers.
    Open {
        url: String,
    },
}

/// Browser box to runner, one per line on its stdout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromBrowser {
    /// Up, and the start-of-run checks are done. `mcp_url` is where the
    /// agent's box reaches the browser tools.
    Ready { mcp_url: String },
    Frame {
        seq: u64,
        /// JPEG, base64.
        jpeg: String,
        width: u32,
        height: u32,
    },
    /// The page shown, as the browser has it (never what the page claims).
    Page { url: String, title: String },
    /// A saved sign-in no longer works: confirmed in a fresh browser loaded
    /// from the saved copy, not the agent's.
    SignedOut { sign_in_id: i64 },
    /// Sign-in mode: what to save. `domains` are the sites it covers;
    /// `expires_at` (Unix seconds) is when its first lasting cookie ends.
    Captured {
        state: serde_json::Value,
        login_url: Option<String>,
        domains: Vec<String>,
        expires_at: Option<i64>,
    },
    /// Something went wrong; the box is ending.
    Failed { why: String },
}

/// Runner to hub, over the relay WebSocket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayUp {
    Frame {
        seq: u64,
        jpeg: String,
        width: u32,
        height: u32,
    },
    Page {
        url: String,
        title: String,
    },
    /// The browser is gone (the run ended, or the sign-in was saved).
    Ended {
        why: String,
    },
}

/// Hub to runner, over the relay WebSocket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayDown {
    /// Someone is watching (or stopped watching).
    Watch {
        on: bool,
        max_width: u32,
    },
    FrameAck {
        seq: u64,
    },
    Input {
        input: Input,
    },
    Control {
        who: Controller,
    },
    /// Sign-in mode: the owner tapped "I'm signed in, save".
    Capture,
}

/// The owner's screen to hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ViewerUp {
    FrameAck { seq: u64 },
    Input { input: Input },
}

/// Hub to the owner's screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ViewerDown {
    Frame {
        seq: u64,
        jpeg: String,
        width: u32,
        height: u32,
    },
    Page {
        url: String,
        title: String,
    },
    Control {
        who: Controller,
    },
    /// The browser is not running (yet, or any more).
    Ended {
        why: String,
    },
}

/// What `/runner/threads/{id}/live` adds when the owner wants to see the
/// run's browser: the session to relay it to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserWant {
    pub session_id: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip() {
        let start = ToBrowser::Start {
            mode: BrowserMode::Run,
            states: vec![SiteState {
                sign_in_id: 3,
                state: serde_json::json!({"cookies": [], "origins": []}),
                check_url: Some("https://staging.example.com/admin".into()),
                login_url: None,
            }],
            start_url: None,
        };
        let line = serde_json::to_string(&start).unwrap_or_default();
        assert!(line.starts_with(r#"{"type":"start","mode":"run""#));
        assert_eq!(serde_json::from_str::<ToBrowser>(&line).ok(), Some(start));
        let click = r#"{"type":"input","input":{"kind":"click","x":0.5,"y":0.25}}"#;
        assert_eq!(
            serde_json::from_str::<ViewerUp>(click).ok(),
            Some(ViewerUp::Input {
                input: Input::Click { x: 0.5, y: 0.25 }
            })
        );
        let out = r#"{"type":"signed_out","sign_in_id":3}"#;
        assert_eq!(
            serde_json::from_str::<FromBrowser>(out).ok(),
            Some(FromBrowser::SignedOut { sign_in_id: 3 })
        );
    }
}
