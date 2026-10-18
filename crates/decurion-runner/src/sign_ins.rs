//! Saved website sign-ins on the PC runner.
//!
//! A claim names the app's saved sign-ins (never the sign-ins themselves).
//! At the start of a run the runner opens each one from this PC's vault,
//! hands it to the run's browser box on its stdin, and hides every cookie
//! and local storage value long enough to matter from what the agent writes,
//! the same way test keys are hidden. What the agent does to those sign-ins
//! is never written back.
//!
//! When the owner signs in through a sign-in window, the runner seals what
//! the browser captured into the vault, then tells the hub which copy is
//! current. The hub only ever learns which sites it covers and when it runs
//! out.

use anyhow::{bail, Context};
use decurion_proto::browser::{FromBrowser, SiteState, MAX_STATE_BYTES};
use decurion_proto::{Claim, Place, SignInRef, SignInSaved, SignInSession};
use decurion_vault::Vault;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

/// Shortest value hidden. Longer than for test keys: a website keeps plenty
/// of ordinary words and settings, and a hit fails the work's review.
pub const MIN_HIDDEN: usize = 16;
/// Longest single value hidden as a whole. Longer values are usually JSON
/// (their pieces are hidden instead); hiding a huge one would cost every
/// line the runner reads and blank out long lines that were cut off.
pub const MAX_HIDDEN: usize = 8 << 10;

const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// What the sign-in calls need from the runner's hub connection.
#[derive(Clone, Copy)]
pub struct HubAccess<'a> {
    pub http: &'a reqwest::Client,
    /// The hub's address, without `/api`.
    pub base: &'a str,
    /// This runner's key.
    pub key: &'a str,
}

impl HubAccess<'_> {
    async fn post<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> anyhow::Result<reqwest::Response> {
        let r = self
            .http
            .post(format!("{}/api{path}", self.base))
            .bearer_auth(self.key)
            .timeout(CALL_TIMEOUT)
            .json(body)
            .send()
            .await
            .with_context(|| format!("hub {path}: unreachable"))?;
        Ok(r)
    }
}

/// The claim's sign-ins, opened from the vault in `data`, ready for the
/// browser box, and the values to hide from the agent's output and keep out
/// of its commits (add them to the run's list beside the test keys).
///
/// Sign-ins stay on the PC: on a cloud worker an app with any is refused,
/// like an app with test keys. A sign-in whose copy is missing or does not
/// open is left out (and logged); report it with [`report_signed_out`] so
/// the owner is asked to sign in again. [`missing`] lists them.
pub fn site_states(
    data: &Path,
    place: Place,
    claim: &Claim,
) -> anyhow::Result<(Vec<SiteState>, Vec<String>)> {
    if claim.sign_ins.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    site_states_in(&Vault::system(data), place, claim)
}

/// [`site_states`] with a given vault, for tests.
pub fn site_states_in(
    vault: &Vault,
    place: Place,
    claim: &Claim,
) -> anyhow::Result<(Vec<SiteState>, Vec<String>)> {
    if place == Place::Cloud && !claim.sign_ins.is_empty() {
        bail!(
            "this app uses saved website sign-ins, which stay on the PC, so its work cannot run \
on the cloud worker yet. Move it to the PC and try again."
        );
    }
    let mut states = Vec::new();
    let mut secrets = Vec::new();
    for r in &claim.sign_ins {
        let opened = match vault.open_sign_in(claim.app.id, r.id, r.version) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                tracing::warn!(
                    sign_in = r.id,
                    "saved sign-in is missing from this PC's vault"
                );
                continue;
            }
            Err(e) => {
                tracing::warn!(sign_in = r.id, error = %e, "saved sign-in did not open");
                continue;
            }
        };
        let state: Value = match serde_json::from_slice(&opened) {
            Ok(v) if looks_like_state(&v) => v,
            _ => {
                tracing::warn!(sign_in = r.id, "saved sign-in is not a browser sign-in");
                continue;
            }
        };
        secrets.extend(hidden_values(&state));
        states.push(SiteState {
            sign_in_id: r.id,
            state,
            check_url: r.check_url.clone(),
            login_url: r.login_url.clone(),
        });
    }
    secrets.sort_by(|a: &String, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    secrets.dedup();
    Ok((states, secrets))
}

/// The claim's sign-ins that [`site_states`] left out.
pub fn missing<'a>(claim: &'a Claim, states: &[SiteState]) -> Vec<&'a SignInRef> {
    claim
        .sign_ins
        .iter()
        .filter(|r| !states.iter().any(|s| s.sign_in_id == r.id))
        .collect()
}

/// Playwright's `storageState`: `cookies` and `origins` lists.
fn looks_like_state(v: &Value) -> bool {
    v.get("cookies").is_some_and(Value::is_array) && v.get("origins").is_some_and(Value::is_array)
}

/// Every cookie and local storage value that looks like a secret (see
/// [`secret_like`]). A value that is JSON (a stored session, say) also gives
/// up the pieces inside it kept under token-like names, and an escaped
/// cookie its plain form, since a page shows them that way. Other pieces of
/// stored JSON (a user's role, a site's public settings) are left alone:
/// they turn up in ordinary code, and a hit there would fail the work.
pub fn hidden_values(state: &Value) -> Vec<String> {
    let mut raw: Vec<&str> = Vec::new();
    for c in state["cookies"].as_array().into_iter().flatten() {
        raw.extend(c["value"].as_str());
    }
    for o in state["origins"].as_array().into_iter().flatten() {
        for item in o["localStorage"].as_array().into_iter().flatten() {
            raw.extend(item["value"].as_str());
        }
    }
    let mut out = Vec::new();
    for v in raw {
        add_value(v, &mut out, 0);
    }
    out
}

fn add_value(v: &str, out: &mut Vec<String>, depth: u8) {
    if v.len() <= MAX_HIDDEN && secret_like(v) {
        out.push(v.to_owned());
        let plain = percent_decoded(v);
        if plain != v && secret_like(&plain) {
            out.push(plain);
        }
    }
    // A value that is itself JSON: hide its tokens, a few levels down.
    if depth < 4 && (v.starts_with('{') || v.starts_with('[')) {
        if let Ok(inner) = serde_json::from_str::<Value>(v) {
            json_strings(&inner, None, out, depth + 1);
        }
    }
}

/// At least [`MIN_HIDDEN`] characters, no spaces, and both letters and
/// digits, as session ids and tokens are; words and plain settings are not.
fn secret_like(v: &str) -> bool {
    v.chars().count() >= MIN_HIDDEN
        && !v.chars().any(char::is_whitespace)
        && v.chars().any(|c| c.is_ascii_digit())
        && v.chars().any(|c| c.is_alphabetic())
}

/// Whether a stored JSON name is one a site keeps a secret under.
fn token_name(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    [
        "token",
        "jwt",
        "secret",
        "session",
        "password",
        "credential",
        "bearer",
        "cookie",
        "sid",
    ]
    .iter()
    .any(|t| k.contains(t))
}

/// The strings in stored JSON kept under token-like names (`key`, the name
/// the value sits under; a list's items take the list's name).
fn json_strings(v: &Value, key: Option<&str>, out: &mut Vec<String>, depth: u8) {
    match v {
        Value::String(s) if key.is_some_and(token_name) => add_value(s, out, depth),
        // Stored JSON inside stored JSON is opened whatever its name.
        Value::String(s) if s.starts_with('{') || s.starts_with('[') => {
            if depth < 4 {
                if let Ok(inner) = serde_json::from_str::<Value>(s) {
                    json_strings(&inner, None, out, depth + 1);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| json_strings(x, key, out, depth)),
        Value::Object(o) => o
            .iter()
            .for_each(|(k, x)| json_strings(x, Some(k), out, depth)),
        _ => {}
    }
}

/// `%XX` escapes turned back into the characters they stand for.
fn percent_decoded(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
}

/// What to add to the agent's starting prompt about the sign-ins its
/// browser has, or `None` without any. Pass the claim's sign-ins and the
/// states [`site_states`] opened: only those are named. The names are the
/// owner's own labels.
pub fn sign_ins_prompt(sign_ins: &[SignInRef], states: &[SiteState]) -> Option<String> {
    let names: Vec<String> = sign_ins
        .iter()
        .filter(|r| states.iter().any(|s| s.sign_in_id == r.id))
        .map(|r| {
            let label: String = r
                .label
                .chars()
                .filter(|c| !c.is_control() && *c != '"')
                .take(60)
                .collect();
            format!("\"{label}\"")
        })
        .collect();
    if names.is_empty() {
        return None;
    }
    Some(format!(
        "Your test browser starts signed in to {} with test accounts the owner saved, so you \
can check pages that need a sign-in. Do not sign out, change those accounts' passwords or \
settings, or copy their cookies or tokens anywhere: they are hidden from what you write, and a \
commit that contains one will not be accepted. If a site asks you to sign in, never type a \
password; say so in your summary, and the owner will sign in again.",
        names.join(", ")
    ))
}

/// Tells the hub a sign-in this run was given no longer works (the browser
/// box confirmed it in a fresh browser). The hub asks the owner to sign in
/// again. Saying so twice is harmless.
///
/// `thread` and `version` name the piece of work and the copy it was given,
/// so with several pieces running at once only that copy can be expired.
pub async fn report_signed_out(
    hub: &HubAccess<'_>,
    sign_in_id: i64,
    thread: i64,
    version: i64,
) -> anyhow::Result<()> {
    let path = format!("/runner/sign-ins/{sign_in_id}/expired");
    let body = serde_json::json!({ "thread": thread, "version": version });
    let r = hub.post(&path, &body).await?;
    if !r.status().is_success() {
        let s = r.status();
        bail!("hub {path}: {s} {}", r.text().await.unwrap_or_default());
    }
    Ok(())
}

/// Saves what a sign-in window captured: sealed into the vault in `data`
/// first, then recorded on the hub as the current copy. Older copies are
/// removed once the hub has it; if the hub refuses, the new copy is removed
/// instead (all of the sign-in, if the owner removed it meanwhile).
/// `captured` is the browser box's [`FromBrowser::Captured`].
pub async fn save_capture(
    hub: &HubAccess<'_>,
    data: &Path,
    session: &SignInSession,
    captured: &FromBrowser,
) -> anyhow::Result<()> {
    save_capture_in(hub, &Vault::system(data), session, captured).await
}

/// [`save_capture`] with a given vault, for tests.
pub async fn save_capture_in(
    hub: &HubAccess<'_>,
    vault: &Vault,
    session: &SignInSession,
    captured: &FromBrowser,
) -> anyhow::Result<()> {
    let FromBrowser::Captured {
        state,
        login_url,
        domains,
        expires_at,
    } = captured
    else {
        bail!("the browser did not send a sign-in to save");
    };
    if !looks_like_state(state) {
        bail!("the browser sent something that is not a sign-in");
    }
    let bytes = serde_json::to_vec(state)?;
    if bytes.len() > MAX_STATE_BYTES {
        bail!(
            "this sign-in is larger than {} MB, too large to save",
            MAX_STATE_BYTES >> 20
        );
    }
    let (app, site, version) = (session.app_id, session.sign_in_id, session.version);
    let v = vault.clone();
    tokio::task::spawn_blocking(move || v.seal_sign_in(app, site, version, &bytes))
        .await
        .context("the key store task failed")?
        .map_err(|e| anyhow::anyhow!("could not save the sign-in on this PC: {e}"))?;

    let path = format!("/runner/sign-ins/{site}/saved");
    let body = SignInSaved {
        session_id: session.session_id,
        domains: domains.clone(),
        expires_at: *expires_at,
        login_url: login_url.clone(),
        version,
    };
    // Unreachable: the hub may or may not have it, so every copy is kept. The
    // hub only ever hands out the version it recorded.
    let r = hub.post(&path, &body).await?;
    let s = r.status();
    let v = vault.clone();
    let tidied = tokio::task::spawn_blocking(move || {
        if s.is_success() {
            v.keep_only_sign_in(app, site, version)
        } else if s == reqwest::StatusCode::NOT_FOUND {
            v.delete_sign_in(app, site)
        } else {
            v.drop_sign_in_version(app, site, version)
        }
    })
    .await;
    match tidied {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, sign_in = site, "could not tidy saved copies"),
        Err(e) => tracing::warn!(error = %e, sign_in = site, "could not tidy saved copies"),
    }
    if !s.is_success() {
        bail!("hub {path}: {s} {}", r.text().await.unwrap_or_default());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use decurion_proto::{Account, App, Profile, Thread, ThreadState};
    use serde_json::json;

    struct Tmp(std::path::PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tmp() -> Tmp {
        Tmp(std::env::temp_dir().join(format!(
            "decurion-runner-sign-ins-{}",
            uuid::Uuid::new_v4().simple()
        )))
    }

    fn claim(app_id: i64, sign_ins: Vec<SignInRef>) -> Claim {
        let app = App {
            id: app_id,
            name: "demo".into(),
            repo_path: "C:\\repos\\demo".into(),
            profile: Profile::A,
            pay_per_use: false,
            monthly_cap_usd: None,
            sites: Vec::new(),
            reviewer: None,
            first_today: false,
            rules: Default::default(),
        };
        let thread = Thread {
            id: 1,
            app_id,
            title: "t".into(),
            prompt: "p".into(),
            state: ThreadState::Running,
            ai: Default::default(),
            session_id: None,
            branch: None,
            place: Place::Pc,
            updated_at: String::new(),
            parent_id: None,
            background: false,
            github: None,
            fix_of: None,
            research: false,
            dismissed_at: None,
        };
        Claim {
            thread,
            app,
            resume: false,
            account: Account::A,
            budget_usd: None,
            messages: vec![],
            interrupted: None,
            test_keys: vec![],
            reserved_micros: None,
            prior_micros: 0,
            checkpoint: None,
            sign_ins,
            context: vec![],
            addons: vec![],
        }
    }

    fn sign_in(id: i64, label: &str) -> SignInRef {
        SignInRef {
            id,
            label: label.into(),
            check_url: Some("https://staging.example.com/admin".into()),
            login_url: None,
            version: 7,
        }
    }

    fn state() -> Value {
        json!({
            "cookies": [
                {"name": "sid", "value": "s3ss10n-abcdef123", "domain": "staging.example.com"},
                {"name": "theme", "value": "dark"},
                {"name": "enc", "value": "a%2Bb%3Dc%3D1234567890ab"},
                {"name": "page", "value": "https://staging.example.com/dashboard"},
            ],
            "origins": [{
                "origin": "https://staging.example.com",
                "localStorage": [
                    {"name": "sb-auth-token", "value": "{\"access_token\":\"eyJhbGciOi.payload1.sig\",\"aud\":\"authenticated\",\"user\":{\"id\":\"short\",\"role\":\"authenticated\"}}"},
                    {"name": "firebase:authUser", "value": "{\"apiKey\":\"AIzaSyA1234567890abcdefgh\",\"stsTokenManager\":{\"accessToken\":\"eyJ0eXAi.fire2.sig\"}}"},
                    {"name": "big", "value": "x".repeat(MAX_HIDDEN + 1)},
                ],
            }],
        })
    }

    #[test]
    fn values_long_enough_are_hidden_including_inside_json() {
        let h = hidden_values(&state());
        assert!(h.contains(&"s3ss10n-abcdef123".to_owned()));
        assert!(h.contains(&"eyJhbGciOi.payload1.sig".to_owned()));
        assert!(h.contains(&"eyJ0eXAi.fire2.sig".to_owned()));
        assert!(h.contains(&"a%2Bb%3Dc%3D1234567890ab".to_owned()));
        assert!(h.contains(&"a+b=c=1234567890ab".to_owned()));
        // Words, settings and public config a site keeps are not secrets,
        // and turn up in ordinary code.
        for plain in [
            "dark",
            "short",
            "authenticated",
            "AIzaSyA1234567890abcdefgh",
            "https://staging.example.com/dashboard",
        ] {
            assert!(!h.iter().any(|v| v == plain), "{plain}");
        }
        // Too long to hide whole; it is not JSON, so nothing inside it either.
        assert!(!h.iter().any(|v| v.len() > MAX_HIDDEN));
        assert_eq!(percent_decoded("100%"), "100%");
        assert_eq!(percent_decoded("%zz%41"), "%zzA");
    }

    #[test]
    fn opens_only_this_apps_sign_ins_and_never_on_the_cloud() {
        let t = tmp();
        let vault = Vault::in_folder(&t.0);
        let bytes = serde_json::to_vec(&state()).expect("json");
        vault.seal_sign_in(3, 5, 7, &bytes).expect("seal");
        // Another app's sign-in with the same number.
        vault.seal_sign_in(4, 6, 7, &bytes).expect("seal other");

        let c = claim(3, vec![sign_in(5, "Staging admin"), sign_in(6, "Gone")]);
        let (states, secrets) = site_states_in(&vault, Place::Pc, &c).expect("open");
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].sign_in_id, 5);
        assert_eq!(states[0].state, state());
        assert_eq!(
            states[0].check_url.as_deref(),
            Some("https://staging.example.com/admin")
        );
        assert!(secrets.contains(&"s3ss10n-abcdef123".to_owned()));
        let gone: Vec<i64> = missing(&c, &states).iter().map(|r| r.id).collect();
        assert_eq!(gone, vec![6]);

        let err = site_states_in(&vault, Place::Cloud, &c).expect_err("cloud");
        assert!(err.to_string().contains("stay on the PC"));
        let none = claim(3, vec![]);
        assert!(site_states_in(&vault, Place::Cloud, &none).is_ok());
    }

    #[test]
    fn the_prompt_names_only_opened_sign_ins_by_label() {
        let refs = vec![sign_in(5, "Staging \"admin\""), sign_in(6, "Gone")];
        let states = vec![SiteState {
            sign_in_id: 5,
            state: state(),
            check_url: None,
            login_url: None,
        }];
        let p = sign_ins_prompt(&refs, &states).expect("prompt");
        assert!(p.contains("\"Staging admin\""));
        assert!(!p.contains("Gone"));
        assert!(!p.contains("s3ss10n"));
        assert!(p.contains("never type a password"));
        assert_eq!(sign_ins_prompt(&refs, &[]), None);
    }

    #[tokio::test]
    async fn a_capture_is_sealed_then_recorded_and_tidied() {
        use axum::extract::Path as UrlPath;
        use axum::http::StatusCode;
        use axum::routing::post;
        use std::sync::{Arc, Mutex};

        let seen: Arc<Mutex<Vec<(i64, Value)>>> = Arc::default();
        let log = seen.clone();
        let app = axum::Router::new().route(
            "/api/runner/sign-ins/{id}/saved",
            post(
                move |UrlPath(id): UrlPath<i64>, axum::Json(b): axum::Json<Value>| {
                    let log = log.clone();
                    async move {
                        log.lock().map(|mut l| l.push((id, b))).ok();
                        match id {
                            5 => StatusCode::OK,
                            6 => StatusCode::NOT_FOUND,
                            _ => StatusCode::CONFLICT,
                        }
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await });
        let base = format!("http://{addr}");
        let http = reqwest::Client::new();
        let hub = HubAccess {
            http: &http,
            base: &base,
            key: "k",
        };

        let t = tmp();
        let vault = Vault::in_folder(&t.0);
        let captured = FromBrowser::Captured {
            state: state(),
            login_url: Some("https://staging.example.com/login".into()),
            domains: vec!["staging.example.com".into()],
            expires_at: Some(1_900_000_000),
        };
        let session = |sign_in_id, version| SignInSession {
            session_id: version,
            sign_in_id,
            app_id: 3,
            start_url: "https://staging.example.com/login".into(),
            version,
        };
        vault.seal_sign_in(3, 5, 1, b"{}").expect("old copy");
        save_capture_in(&hub, &vault, &session(5, 2), &captured)
            .await
            .expect("saved");
        let opened = vault.open_sign_in(3, 5, 2).expect("open").expect("there");
        assert_eq!(serde_json::from_slice::<Value>(&opened).ok(), Some(state()));
        assert!(!vault.sign_in_file(3, 5, 1).exists());
        {
            let l = seen.lock().expect("log");
            assert_eq!(l[0].0, 5);
            assert_eq!(l[0].1["session_id"], 2);
            assert_eq!(l[0].1["version"], 2);
            assert_eq!(l[0].1["expires_at"], 1_900_000_000);
            assert_eq!(l[0].1["domains"], json!(["staging.example.com"]));
            // The sign-in itself never goes to the hub.
            assert!(!l[0].1.to_string().contains("s3ss10n"));
        }

        // Removed meanwhile: nothing of it is left.
        assert!(save_capture_in(&hub, &vault, &session(6, 3), &captured)
            .await
            .is_err());
        assert!(!vault.sign_in_file(3, 6, 3).exists());
        // Refused for another reason: only the new copy goes.
        vault.seal_sign_in(3, 7, 1, b"{}").expect("old copy");
        assert!(save_capture_in(&hub, &vault, &session(7, 4), &captured)
            .await
            .is_err());
        assert!(!vault.sign_in_file(3, 7, 4).exists());
        assert!(vault.sign_in_file(3, 7, 1).exists());

        let not_captured = FromBrowser::Failed { why: "x".into() };
        assert!(save_capture_in(&hub, &vault, &session(5, 9), &not_captured)
            .await
            .is_err());
    }
}
