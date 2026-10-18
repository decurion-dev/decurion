//! The runner's side of the browser box's lines (see
//! [`decurion_proto::browser`]): what it writes on the box's stdin, what it
//! accepts from its stdout, and how the hub's relay maps onto both. Nothing
//! here does any I/O, so it can be tested directly; `main.rs` does the rest.
//!
//! The browser box runs web pages, and the agent drives it, so everything it
//! prints is checked and capped before the runner acts on it or passes it on.

use decurion_proto::browser::{
    BrowserMode, FromBrowser, Input, RelayDown, RelayUp, SiteState, ToBrowser, MAX_FRAME_BYTES,
    MAX_INPUT_BYTES, MAX_STATE_BYTES,
};

/// The most saved sign-ins one run's browser takes.
pub const MAX_STATES: usize = 64;
/// The longest line the runner writes: a start line carries every saved
/// sign-in, each at most [`MAX_STATE_BYTES`].
pub const MAX_TO_LINE: usize = MAX_STATES * (MAX_STATE_BYTES + (16 << 10));
/// The longest line the box may print: a saved sign-in, with room for the
/// rest of the line. A screen frame is smaller.
pub const MAX_FROM_LINE: usize = MAX_STATE_BYTES + (64 << 10);
/// JPEG quality asked of the screencast. The box lowers it on its own when a
/// frame would weigh more than [`MAX_FRAME_BYTES`].
pub const QUALITY: u8 = 60;
/// Frame widths the runner asks for, whatever the viewer says.
pub const MIN_WIDTH: u32 = 320;
pub const MAX_WIDTH: u32 = 1920;
/// Longest address and title passed on to the owner's screen.
pub const MAX_URL: usize = 8 << 10;
pub const MAX_TITLE: usize = 1 << 10;
/// Longest reason for a failure kept from the box.
pub const MAX_WHY: usize = 2000;
/// Most sites one saved sign-in may cover.
pub const MAX_DOMAINS: usize = 500;
/// The named keys the owner's screen may send.
pub const KEYS: [&str; 8] = [
    "Enter",
    "Tab",
    "Backspace",
    "Escape",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
];

/// The first line for a run's browser, with the app's saved sign-ins.
pub fn start_run(states: Vec<SiteState>) -> ToBrowser {
    ToBrowser::Start {
        mode: BrowserMode::Run,
        states,
        start_url: None,
    }
}

/// The first line for a sign-in window, which opens at `start_url`.
pub fn start_sign_in(start_url: &str) -> anyhow::Result<ToBrowser> {
    if !web_address(start_url) {
        anyhow::bail!("the sign-in page must be a web address starting with https:// or http://");
    }
    Ok(ToBrowser::Start {
        mode: BrowserMode::SignIn,
        states: Vec::new(),
        start_url: Some(start_url.to_owned()),
    })
}

/// One line for the box's stdin, newline included. Refuses lines the box
/// would refuse anyway: too many or too big sign-ins, odd input.
pub fn encode(msg: &ToBrowser) -> anyhow::Result<String> {
    match msg {
        ToBrowser::Start { states, .. } => {
            if states.len() > MAX_STATES {
                anyhow::bail!("at most {MAX_STATES} saved sign-ins go to one browser");
            }
            for s in states {
                if serde_json::to_string(&s.state)?.len() > MAX_STATE_BYTES {
                    anyhow::bail!("saved sign-in {} is too big", s.sign_in_id);
                }
            }
        }
        ToBrowser::Input { input } if !input_ok(input) => anyhow::bail!("odd input refused"),
        _ => {}
    }
    let mut line = serde_json::to_string(msg)?;
    if line.len() > MAX_TO_LINE {
        anyhow::bail!("line for the browser is too long");
    }
    line.push('\n');
    Ok(line)
}

/// Reads one line the box printed. Anything malformed or oversized is
/// refused; addresses, titles and reasons are cut to size.
pub fn decode(line: &str) -> Result<FromBrowser, String> {
    if line.len() > MAX_FROM_LINE {
        return Err(format!("line of {} bytes is too long", line.len()));
    }
    let msg: FromBrowser =
        serde_json::from_str(line.trim_end()).map_err(|e| format!("not a browser line: {e}"))?;
    Ok(match msg {
        FromBrowser::Ready { mcp_url } => {
            // The tools are only ever served inside the box's own network.
            if !mcp_url.starts_with("http://127.0.0.1:") || mcp_url.len() > 100 {
                return Err("the browser tools are not on localhost".into());
            }
            FromBrowser::Ready { mcp_url }
        }
        FromBrowser::Frame {
            seq,
            jpeg,
            width,
            height,
        } => {
            if jpeg.len() > MAX_FRAME_BYTES {
                return Err(format!("frame of {} bytes is too big", jpeg.len()));
            }
            if !(1..=8192).contains(&width) || !(1..=8192).contains(&height) {
                return Err("frame has an odd size".into());
            }
            FromBrowser::Frame {
                seq,
                jpeg,
                width,
                height,
            }
        }
        FromBrowser::Page { url, title } => FromBrowser::Page {
            url: clip(&url, MAX_URL),
            title: clip(&title, MAX_TITLE),
        },
        FromBrowser::Captured {
            state,
            login_url,
            domains,
            expires_at,
        } => {
            let size = serde_json::to_string(&state).map_or(usize::MAX, |s| s.len());
            if size > MAX_STATE_BYTES {
                return Err(format!("sign-in of {size} bytes is too big to save"));
            }
            if !state.is_object() {
                return Err("sign-in is not a browser storage state".into());
            }
            if domains.len() > MAX_DOMAINS || domains.iter().any(|d| d.len() > 253) {
                return Err("sign-in covers too many sites".into());
            }
            FromBrowser::Captured {
                state,
                login_url: login_url.filter(|u| u.len() <= MAX_URL && web_address(u)),
                domains,
                expires_at,
            }
        }
        FromBrowser::Failed { why } => FromBrowser::Failed {
            why: clip(&why, MAX_WHY),
        },
        m @ FromBrowser::SignedOut { .. } => m,
    })
}

/// Whether one piece of the owner's input is something the box takes:
/// positions on the frame, short text, a named key, a web address.
pub fn input_ok(i: &Input) -> bool {
    let frac = |v: f64| v.is_finite() && (0.0..=1.0).contains(&v);
    match i {
        Input::Click { x, y } => frac(*x) && frac(*y),
        Input::Scroll { x, y, dx, dy } => {
            let far = |v: f64| v.is_finite() && v.abs() <= 10.0;
            frac(*x) && frac(*y) && far(*dx) && far(*dy)
        }
        Input::Text { text } => !text.is_empty() && text.len() <= MAX_INPUT_BYTES,
        Input::Key { key } => KEYS.contains(&key.as_str()),
        Input::Go { url } => url.len() <= MAX_INPUT_BYTES && web_address(url),
        Input::Back => true,
    }
}

/// An http or https address with nothing odd in it.
pub fn web_address(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && url.len() > "http://".len()
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// What the hub asked for, as a line for the box. `None` for anything that
/// does not apply (saving a sign-in during a run) or does not pass the checks.
pub fn from_relay(d: RelayDown, mode: BrowserMode) -> Option<ToBrowser> {
    Some(match d {
        RelayDown::Watch { on, max_width } => ToBrowser::Screencast {
            on,
            max_width: max_width.clamp(MIN_WIDTH, MAX_WIDTH),
            quality: QUALITY,
        },
        RelayDown::FrameAck { seq } => ToBrowser::FrameAck { seq },
        RelayDown::Input { input } if input_ok(&input) => ToBrowser::Input { input },
        RelayDown::Input { .. } => return None,
        RelayDown::Control { who } => ToBrowser::Control { who },
        RelayDown::Capture if mode == BrowserMode::SignIn => ToBrowser::Capture,
        RelayDown::Capture => return None,
    })
}

/// What of the box's output goes to the hub: the screen and its address.
pub fn to_relay(f: &FromBrowser) -> Option<RelayUp> {
    match f {
        FromBrowser::Frame {
            seq,
            jpeg,
            width,
            height,
        } => Some(RelayUp::Frame {
            seq: *seq,
            jpeg: jpeg.clone(),
            width: *width,
            height: *height,
        }),
        FromBrowser::Page { url, title } => Some(RelayUp::Page {
            url: url.clone(),
            title: title.clone(),
        }),
        _ => None,
    }
}

/// Where the runner opens the relay for a browser session: the hub's own
/// address with ws:// or wss:// in place of http:// or https://.
pub fn relay_url(hub_base: &str, session_id: i64) -> anyhow::Result<String> {
    let rest = if let Some(r) = hub_base.strip_prefix("https://") {
        format!("wss://{r}")
    } else if let Some(r) = hub_base.strip_prefix("http://") {
        format!("ws://{r}")
    } else {
        anyhow::bail!("hub address {hub_base:?} must start with https:// or http://");
    };
    Ok(format!(
        "{}/api/runner/browser/{session_id}/ws",
        rest.trim_end_matches('/')
    ))
}

/// At most `n` bytes of `s`, cut at a character boundary.
fn clip(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_owned();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use decurion_proto::browser::Controller;
    use serde_json::json;

    fn state(id: i64, cookie: &str) -> SiteState {
        SiteState {
            sign_in_id: id,
            state: json!({"cookies": [{"name": "sid", "value": cookie, "domain": "example.com",
                "path": "/", "expires": -1, "httpOnly": true, "secure": true, "sameSite": "Lax"}],
                "origins": []}),
            check_url: Some("https://example.com/admin".into()),
            login_url: Some("https://example.com/login".into()),
        }
    }

    #[test]
    fn a_start_line_round_trips_and_ends_the_line() {
        let start = start_run(vec![state(3, "abcdefgh12345678")]);
        let line = encode(&start).unwrap_or_default();
        assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
        assert_eq!(serde_json::from_str::<ToBrowser>(&line).ok(), Some(start));
        let sign_in = start_sign_in("https://example.com/login").ok();
        assert!(matches!(
            sign_in,
            Some(ToBrowser::Start { mode: BrowserMode::SignIn, ref start_url, .. })
                if start_url.as_deref() == Some("https://example.com/login")
        ));
        assert!(start_sign_in("file:///etc/passwd").is_err());
        assert!(start_sign_in("javascript:alert(1)").is_err());
    }

    #[test]
    fn oversized_sign_ins_are_not_sent() {
        let big = "x".repeat(MAX_STATE_BYTES);
        assert!(encode(&start_run(vec![state(1, &big)])).is_err());
        let many: Vec<SiteState> = (0..=MAX_STATES as i64).map(|i| state(i, "v")).collect();
        assert!(encode(&start_run(many)).is_err());
    }

    #[test]
    fn the_box_lines_are_checked_and_capped() {
        let ok = r#"{"type":"ready","mcp_url":"http://127.0.0.1:8931/mcp"}"#;
        assert!(matches!(decode(ok), Ok(FromBrowser::Ready { .. })));
        let away = r#"{"type":"ready","mcp_url":"http://203.0.113.9:8931/mcp"}"#;
        assert!(decode(away).is_err());
        assert!(decode("not json").is_err());
        assert!(decode(&"x".repeat(MAX_FROM_LINE + 1)).is_err());

        let frame = |jpeg: String, w: u32| {
            serde_json::to_string(&FromBrowser::Frame {
                seq: 1,
                jpeg,
                width: w,
                height: 600,
            })
            .unwrap_or_default()
        };
        assert!(decode(&frame("/9j/".into(), 800)).is_ok());
        assert!(decode(&frame("A".repeat(MAX_FRAME_BYTES + 1), 800)).is_err());
        assert!(decode(&frame("/9j/".into(), 0)).is_err());

        let page = serde_json::to_string(&FromBrowser::Page {
            url: format!("https://example.com/{}", "a".repeat(MAX_URL)),
            title: "é".repeat(MAX_TITLE),
        })
        .unwrap_or_default();
        match decode(&page) {
            Ok(FromBrowser::Page { url, title }) => {
                assert!(url.len() <= MAX_URL && title.len() <= MAX_TITLE);
            }
            other => panic!("{other:?}"),
        }

        let captured = |state: serde_json::Value, login: &str| {
            serde_json::to_string(&FromBrowser::Captured {
                state,
                login_url: Some(login.into()),
                domains: vec!["example.com".into()],
                expires_at: Some(1_800_000_000),
            })
            .unwrap_or_default()
        };
        let good = captured(
            json!({"cookies": [], "origins": []}),
            "https://example.com/login",
        );
        assert!(matches!(
            decode(&good),
            Ok(FromBrowser::Captured {
                login_url: Some(_),
                ..
            })
        ));
        // A login page that is not a web address is dropped, not trusted.
        let odd = captured(json!({"cookies": [], "origins": []}), "javascript:1");
        assert!(matches!(
            decode(&odd),
            Ok(FromBrowser::Captured {
                login_url: None,
                ..
            })
        ));
        let huge = captured(
            json!({"cookies": [], "origins": [{"origin": "https://example.com",
                "localStorage": [{"name": "k", "value": "x".repeat(MAX_STATE_BYTES)}]}]}),
            "https://example.com/login",
        );
        assert!(decode(&huge).is_err());
        assert!(decode(&captured(json!("text"), "https://example.com/")).is_err());
    }

    #[test]
    fn only_sensible_input_reaches_the_browser() {
        assert!(input_ok(&Input::Click { x: 0.5, y: 1.0 }));
        assert!(!input_ok(&Input::Click { x: 1.5, y: 0.2 }));
        assert!(!input_ok(&Input::Click {
            x: f64::NAN,
            y: 0.2
        }));
        assert!(input_ok(&Input::Scroll {
            x: 0.5,
            y: 0.5,
            dx: 0.0,
            dy: -0.3
        }));
        assert!(!input_ok(&Input::Scroll {
            x: 0.5,
            y: 0.5,
            dx: 0.0,
            dy: f64::INFINITY
        }));
        assert!(input_ok(&Input::Text { text: "hi".into() }));
        assert!(!input_ok(&Input::Text {
            text: "x".repeat(MAX_INPUT_BYTES + 1)
        }));
        assert!(input_ok(&Input::Key {
            key: "Enter".into()
        }));
        assert!(!input_ok(&Input::Key {
            key: "Control".into()
        }));
        assert!(input_ok(&Input::Go {
            url: "https://example.com/".into()
        }));
        for bad in [
            "file:///etc/passwd",
            "chrome://settings",
            "javascript:x",
            "https://a b",
        ] {
            assert!(!input_ok(&Input::Go { url: bad.into() }), "{bad}");
        }
        let line = encode(&ToBrowser::Input {
            input: Input::Go {
                url: "about:blank".into(),
            },
        });
        assert!(line.is_err());
    }

    #[test]
    fn the_relay_maps_onto_the_box() {
        assert_eq!(
            from_relay(
                RelayDown::Watch {
                    on: true,
                    max_width: 100_000
                },
                BrowserMode::Run
            ),
            Some(ToBrowser::Screencast {
                on: true,
                max_width: MAX_WIDTH,
                quality: QUALITY
            })
        );
        assert_eq!(
            from_relay(
                RelayDown::Control {
                    who: Controller::Owner
                },
                BrowserMode::Run
            ),
            Some(ToBrowser::Control {
                who: Controller::Owner
            })
        );
        // Saving a sign-in only happens in a sign-in window.
        assert_eq!(from_relay(RelayDown::Capture, BrowserMode::Run), None);
        assert_eq!(
            from_relay(RelayDown::Capture, BrowserMode::SignIn),
            Some(ToBrowser::Capture)
        );
        let odd = RelayDown::Input {
            input: Input::Click { x: 2.0, y: 0.0 },
        };
        assert_eq!(from_relay(odd, BrowserMode::Run), None);
        // Only the screen and its address go up; sign-ins never do.
        assert!(to_relay(&FromBrowser::SignedOut { sign_in_id: 1 }).is_none());
        assert!(to_relay(&FromBrowser::Captured {
            state: json!({}),
            login_url: None,
            domains: vec![],
            expires_at: None
        })
        .is_none());
        assert!(matches!(
            to_relay(&FromBrowser::Page {
                url: "https://example.com/".into(),
                title: "Example".into()
            }),
            Some(RelayUp::Page { .. })
        ));
    }

    #[test]
    fn the_relay_address_follows_the_hub() {
        assert_eq!(
            relay_url("https://hub.example.com", 9).unwrap_or_default(),
            "wss://hub.example.com/api/runner/browser/9/ws"
        );
        assert_eq!(
            relay_url("http://127.0.0.1:7420/", 9).unwrap_or_default(),
            "ws://127.0.0.1:7420/api/runner/browser/9/ws"
        );
        assert!(relay_url("ftp://x", 9).is_err());
    }
}
