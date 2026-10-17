//! Phone notifications through the browser's own push service (Web Push with
//! VAPID). The hub signs each message with a key kept in its data folder; the
//! phone's browser decrypts it. Notifications carry only a short line, never
//! agent output.

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

#[derive(serde::Serialize)]
struct Note<'a> {
    title: &'a str,
    body: &'a str,
    url: &'a str,
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
        let resp = r
            .send()
            .await
            .map_err(|e| SendError::Other(e.to_string()))?;
        match resp.status().as_u16() {
            200..=299 => Ok(()),
            404 | 410 => Err(SendError::Gone),
            code => Err(SendError::Other(format!("push service returned {code}"))),
        }
    }

    /// Sends one short notification to every signed-in device that turned
    /// notifications on. Subscriptions the push service no longer knows are
    /// dropped. Never fails the caller.
    pub async fn notify(&self, store: &Store, title: &str, body: &str) {
        let Ok(targets) = store.push_targets().await else {
            return;
        };
        let Ok(payload) = serde_json::to_vec(&Note {
            title,
            body,
            url: "/inbox",
        }) else {
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
                Err(SendError::Other(e)) => tracing::warn!(device, error = %e, "push failed"),
            }
        }
    }
}

enum SendError {
    Gone,
    Other(String),
}

/// What a state change means for the owner, if it is worth a buzz.
pub fn message_for(state: decurion_proto::ThreadState, title: &str) -> Option<(String, String)> {
    use decurion_proto::ThreadState::*;
    let head = match state {
        InReview => "Ready for review",
        NeedsYou => "Waiting on you",
        Failed => "Stopped",
        _ => return None,
    };
    let title: String = title.chars().take(80).collect();
    Some((head.to_owned(), title))
}
