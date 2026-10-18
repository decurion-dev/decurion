//! Phone notifications through the browser's own push service (Web Push with
//! VAPID). The hub signs each message with a key kept in its data folder; the
//! phone's browser decrypts it. Notifications carry only a short line, never
//! agent output, and every title starts with the app's name.
//!
//! A notice may carry one thing it can do from the phone's lock screen:
//! approve the exact plan it is about, accept the exact work it is about, or
//! open a pull request on GitHub. The service worker (`sw.js`) makes that tap
//! through the same owner routes the inbox uses, with the device's own
//! sign-in cookie and the receipt sent here, so a notice holds no key and a
//! tap on two phones still acts once.

use crate::store::Store;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use std::path::Path;
use std::sync::Arc;
use web_push_native::jwt_simple::algorithms::{ECDSAP256PublicKeyLike, ES256KeyPair};
use web_push_native::{p256, Auth, WebPushBuilder};

const CONTACT: &str = "https://github.com/decurion-dev/decurion";

#[derive(Clone)]
pub struct Pusher {
    key: Arc<ES256KeyPair>,
    http: reqwest::Client,
}

/// The longest title or body line a notice carries.
const LINE_CHARS: usize = 80;

/// Something the owner should hear about on their phone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// The app it is about, or empty for news about no one app.
    pub app: String,
    /// "Refill tracker: ready for review".
    pub title: String,
    /// Which piece of work or goal, by its title.
    pub body: String,
    /// Where a plain tap goes. Checked again when sent ([`tap_url_ok`]).
    pub url: String,
    /// Notices about the same goal or work replace each other on the phone;
    /// different ones stay side by side, so a plan's Approve button is not
    /// pushed off by news about another app.
    pub tag: String,
    pub act: Option<Act>,
}

/// The one thing a notice can do by itself.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Act {
    /// Approve plan `plan` of goal `goal`. A newer plan makes the tap
    /// fail, so it never approves something the owner was not told about.
    Approve { goal: i64, plan: i64 },
    /// Accept work `thread`, only while it is still waiting for review.
    Accept { thread: i64 },
    /// Open the notice's pull request on GitHub.
    OpenPr,
}

impl Notice {
    /// A notice with no app behind it ("" app) gets a capitalized title.
    pub fn new(app: &str, head: &str, body: &str) -> Self {
        let title = if app.trim().is_empty() {
            let mut c = head.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        } else {
            format!("{}: {head}", app.trim())
        };
        Self {
            app: clip(app.trim()),
            title: clip(&title),
            body: clip(body),
            url: "/inbox".to_owned(),
            tag: "decurion".to_owned(),
            act: None,
        }
    }

    /// About goal `goal`; a tap opens it on the Goals page.
    pub fn goal(app: &str, goal: i64, head: &str, title: &str) -> Self {
        Self {
            url: format!("/goals#goal-{goal}"),
            tag: format!("goal-{goal}"),
            ..Self::new(app, head, title)
        }
    }

    /// About piece of work `thread`; a tap opens the inbox.
    pub fn work(app: &str, thread: i64, head: &str, title: &str) -> Self {
        Self {
            tag: format!("work-{thread}"),
            ..Self::new(app, head, title)
        }
    }

    /// Adds Approve for this exact plan.
    pub fn approve(self, goal: i64, plan: i64) -> Self {
        Self {
            act: Some(Act::Approve { goal, plan }),
            ..self
        }
    }

    /// Adds Accept for this exact work.
    pub fn accept(self, thread: i64) -> Self {
        Self {
            act: Some(Act::Accept { thread }),
            ..self
        }
    }

    /// Opens `url` on GitHub, with its own button. Anything that is not a
    /// GitHub address is left out.
    pub fn open_pr(self, url: &str) -> Self {
        if !url.starts_with("https://github.com/") || !tap_url_ok(url) {
            return self;
        }
        Self {
            url: url.to_owned(),
            act: Some(Act::OpenPr),
            ..self
        }
    }

    /// What the phone receives. `receipt` goes with a tap that changes
    /// something, so the hub acts on it once however many phones tap.
    fn payload(&self, receipt: &str) -> serde_json::Value {
        let url = if tap_url_ok(&self.url) {
            self.url.as_str()
        } else {
            "/inbox"
        };
        let mut v = serde_json::json!({
            "app": self.app,
            "title": self.title,
            "body": self.body,
            "url": url,
            "tag": self.tag,
        });
        match &self.act {
            Some(Act::OpenPr) if url == "/inbox" => {}
            Some(a) => {
                v["act"] = serde_json::to_value(a).unwrap_or_default();
                if !matches!(a, Act::OpenPr) {
                    v["receipt"] = receipt.into();
                }
            }
            None => {}
        }
        v
    }
}

fn clip(s: &str) -> String {
    s.chars().take(LINE_CHARS).collect()
}

impl Pusher {
    /// Loads the signing key from `vapid.key` in `dir`, creating it on first use.
    pub fn load_or_create(dir: &Path) -> anyhow::Result<Self> {
        let path = dir.join("vapid.key");
        let key = if path.exists() {
            ES256KeyPair::from_bytes(&std::fs::read(&path)?)?
        } else {
            let k = ES256KeyPair::generate();
            crate::write_atomic(&path, &k.to_bytes())?;
            k
        };
        // A push service that hangs or redirects must not stall the others.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            key: Arc::new(key),
            http,
        })
    }

    /// The public key a browser needs to subscribe (base64url, uncompressed point).
    pub fn public_key(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.key.public_key().public_key().to_bytes_uncompressed())
    }

    async fn send_one(
        &self,
        s: &crate::devices::PushSubscription,
        body: &[u8],
    ) -> Result<(), SendError> {
        let endpoint = s.endpoint.parse().map_err(|_| SendError::Gone)?;
        let ua_public = URL_SAFE_NO_PAD
            .decode(s.p256dh.trim_end_matches('='))
            .ok()
            .and_then(|b| p256::PublicKey::from_sec1_bytes(&b).ok())
            .ok_or(SendError::Gone)?;
        let auth = URL_SAFE_NO_PAD
            .decode(s.auth.trim_end_matches('='))
            .ok()
            .filter(|b| b.len() == 16)
            .ok_or(SendError::Gone)?;
        let req = WebPushBuilder::new(endpoint, ua_public, Auth::clone_from_slice(&auth))
            .with_vapid(&self.key, CONTACT)
            .build(body.to_vec())
            .map_err(|e| SendError::Other(e.to_string()))?;
        let (parts, payload) = req.into_parts();
        let mut r = self.http.post(parts.uri.to_string()).body(payload);
        for (k, v) in parts.headers.iter() {
            r = r.header(k.as_str(), v.as_bytes());
        }
        let mut resp = r
            .send()
            .await
            .map_err(|e| SendError::Other(e.to_string()))?;
        match resp.status().as_u16() {
            200..=299 => Ok(()),
            404 | 410 => Err(SendError::Gone),
            // Most often the phone made this subscription with an older hub
            // key; the inbox replaces it the next time it is opened. It can
            // also be the hub's own fault (a wrong clock spoils the signature),
            // so the subscription is kept: dropping it would cut off every
            // phone at once. The push service's answer goes in the log.
            code @ (401 | 403) => Err(SendError::Refused {
                code,
                said: first_bytes(&mut resp, REFUSAL_SHOWN).await,
            }),
            code => Err(SendError::Other(format!("push service returned {code}"))),
        }
    }

    /// Sends one short notification, opening the inbox, to every signed-in
    /// device that turned notifications on. Never fails the caller.
    pub async fn notify(&self, store: &Store, title: &str, body: &str) {
        let n = Notice {
            title: clip(title),
            body: clip(body),
            ..Notice::new("", "", "")
        };
        self.send(store, &n).await;
    }

    /// Sends `n` to every signed-in device that turned notifications on.
    /// Subscriptions the push service no longer knows are dropped. A tap can
    /// only open one of the hub's pages or GitHub ([`tap_url_ok`]), so a
    /// notification can never send the owner anywhere else. Never fails the
    /// caller.
    pub async fn send(&self, store: &Store, n: &Notice) {
        let Ok(targets) = store.push_targets().await else {
            return;
        };
        let Ok(payload) = serde_json::to_vec(&n.payload(&crate::new_secret())) else {
            return;
        };
        let mut sends = tokio::task::JoinSet::new();
        for (device, sub) in targets {
            let me = self.clone();
            let payload = payload.clone();
            sends.spawn(async move {
                let r = me.send_one(&sub, &payload).await;
                (device, sub.endpoint, r)
            });
        }
        while let Some(Ok((device, endpoint, result))) = sends.join_next().await {
            match result {
                Ok(()) => {}
                Err(SendError::Gone) => {
                    let _ = store.drop_push(device, &endpoint).await;
                }
                Err(SendError::Refused { code, said }) => {
                    tracing::warn!(
                        device,
                        status = code,
                        said = %said,
                        "push service refused this phone's subscription"
                    );
                }
                Err(SendError::Other(e)) => tracing::warn!(device, error = %e, "push failed"),
            }
        }
    }
}

/// Where a notification may take the owner: one of the hub's own pages, or
/// GitHub.
fn tap_url_ok(url: &str) -> bool {
    let local = url.starts_with('/') && !url.starts_with("//") && !url.contains('\\');
    let github = url.starts_with("https://github.com/");
    (local || github) && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

enum SendError {
    Gone,
    /// 401 or 403: the push service refused it. `said` is the start of the
    /// push service's answer, for the log.
    Refused {
        code: u16,
        said: String,
    },
    Other(String),
}

/// How much of a refusal's answer goes in the log. The answer comes from the
/// push service, never holds the subscription's keys, and is cut short so a
/// large one cannot flood the log.
const REFUSAL_SHOWN: usize = 200;

/// Up to `max` bytes of the answer, as text with control characters removed.
/// Reads no further than needed, and an unreadable answer is just empty.
async fn first_bytes(resp: &mut reqwest::Response, max: usize) -> String {
    let mut got = Vec::new();
    while got.len() < max {
        match resp.chunk().await {
            Ok(Some(c)) => got.extend_from_slice(&c),
            _ => break,
        }
    }
    got.truncate(max);
    String::from_utf8_lossy(&got)
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

/// What an agent moving work means for the owner, if it is worth a buzz.
/// Work ready for review can be accepted from the notice.
pub fn for_move(t: &decurion_proto::Thread, app: &str) -> Option<Notice> {
    use decurion_proto::ThreadState::*;
    let head = match t.state {
        InReview => "ready for review",
        NeedsYou => "waiting on you",
        Failed => "stopped",
        _ => return None,
    };
    let n = Notice::work(app, t.id, head, &t.title);
    Some(if t.state == InReview {
        n.accept(t.id)
    } else {
        n
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_notice_starts_with_the_app() {
        let n = Notice::work("Refill tracker", 7, "ready for review", "Add reminders");
        assert_eq!(n.title, "Refill tracker: ready for review");
        assert_eq!(n.body, "Add reminders");
        assert_eq!(n.tag, "work-7");
        // News about no one app still reads as a sentence.
        assert_eq!(
            Notice::new("", "spending cap reached", "x").title,
            "Spending cap reached"
        );
        let long = "x".repeat(200);
        assert_eq!(
            Notice::new("a", "b", &long).body.chars().count(),
            LINE_CHARS
        );
    }

    #[test]
    fn only_taps_that_change_something_carry_the_receipt() {
        let n = Notice::goal("Atlas", 3, "plan ready to approve", "Maps").approve(3, 41);
        let v = n.payload("r-1234567890");
        assert_eq!(v["url"], "/goals#goal-3");
        assert_eq!(v["tag"], "goal-3");
        assert_eq!(v["app"], "Atlas");
        assert_eq!(
            v["act"],
            serde_json::json!({"kind": "approve", "goal": 3, "plan": 41})
        );
        assert_eq!(v["receipt"], "r-1234567890");

        let v = Notice::work("Atlas", 5, "ready for review", "Maps")
            .accept(5)
            .payload("r-1234567890");
        assert_eq!(v["act"], serde_json::json!({"kind": "accept", "thread": 5}));
        assert_eq!(v["receipt"], "r-1234567890");

        let pr = "https://github.com/acme/atlas/pull/9";
        let v = Notice::work("Atlas", 5, "ready to merge on GitHub", "Maps")
            .open_pr(pr)
            .payload("r-1234567890");
        assert_eq!(v["url"], pr);
        assert_eq!(v["act"], serde_json::json!({"kind": "open_pr"}));
        assert!(v.get("receipt").is_none());

        // Plain news has nothing to tap but the notice itself.
        let v = Notice::new("Atlas", "stopped", "Maps").payload("r-1234567890");
        assert!(v.get("act").is_none() && v.get("receipt").is_none());
    }

    #[test]
    fn open_pr_takes_only_github() {
        for bad in [
            "/inbox",
            "https://evil.example/pull/1",
            "http://github.com/a/b",
        ] {
            let n = Notice::work("Atlas", 5, "sent", "Maps").open_pr(bad);
            assert_eq!(n.act, None, "{bad}");
            assert_eq!(n.url, "/inbox");
        }
    }

    #[test]
    fn a_tap_opens_only_the_hub_or_github() {
        assert!(tap_url_ok("/inbox"));
        assert!(tap_url_ok("https://github.com/acme/shop/pull/7"));
        for bad in [
            "//evil.example/x",
            "https://evil.example/x",
            "https://github.com.evil.example/x",
            "http://github.com/acme/shop",
            "javascript:alert(1)",
            "/in box",
            "/\\evil.example",
        ] {
            assert!(!tap_url_ok(bad), "{bad}");
        }
    }
}
