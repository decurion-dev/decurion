//! The Sign-ins list in Settings: every account Decurion signs in with, on
//! one page, each with a green, amber or red light and when it runs out,
//! where that can be known. Claude A and B, the GitHub token, how phones
//! reach the hub, each paired phone or browser, each cloud worker and each
//! website sign-in. A reminder goes to the phone a week before anything
//! with a known date runs out.
//!
//! What must stay true:
//! - Nothing here shows or logs a secret. Claude sign-ins and the GitHub
//!   token are only checked for being there (and, for GitHub, asked about
//!   once an hour); the list carries names, lights, dates and plain words.
//! - The light is worked out here, once, so the page, the phone and the
//!   reminders always agree.
//! - Reminders are sent once per sign-in and date (`sign_in_reminders`), and
//!   say only the owner's own names and the date.

use crate::claude_sign_in;
use crate::github::TokenCheck;
use crate::sign_ins::SignInStatus;
use crate::store::{Result, Store};
use crate::{forbidden, ApiError, AppState, Caller};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use decurion_proto::Profile;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Anything that runs out within this many days is amber, and gets a reminder.
pub const WARN_DAYS: i64 = 7;
/// Phones and browsers stay signed in while used; one unused this long is
/// signed out (its cookie lasts a year from its last use).
const DEVICE_DAYS: i64 = 365;
/// How often GitHub is asked about the token.
const ASK_GITHUB_EVERY: Duration = Duration::from_secs(60 * 60);
/// How often reminders are looked for.
const REMIND_EVERY: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    Green,
    Amber,
    Red,
    /// Not set up, and nothing needs it yet.
    Grey,
}

/// What a row's button does on the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// "Sign in on this PC" for `profile`.
    ClaudeSignIn,
    /// Save or replace the GitHub token.
    GithubToken,
    /// Connect phones to the hub, then pair one.
    ConnectPhone,
    /// Sign in to the website sign-in `id` again.
    SiteSignIn,
    /// Add a cloud worker.
    AddCloud,
}

/// One row of the Sign-ins list.
#[derive(Debug, Clone, Serialize)]
pub struct Light {
    /// Stable within the list, e.g. `claude-A`, `device-3`, `site-5`.
    pub key: String,
    /// `claude`, `github`, `phone`, `cloud` or `site`.
    pub group: &'static str,
    pub label: String,
    pub light: Color,
    /// What is going on, in plain words, without the date (the page writes
    /// dates in the owner's own format).
    pub text: String,
    pub expires_at: Option<String>,
    /// Whole days until `expires_at`; negative once it has passed.
    pub days_left: Option<i64>,
    pub action: Option<Action>,
    pub profile: Option<Profile>,
    pub id: Option<i64>,
}

impl Light {
    fn new(key: String, group: &'static str, label: String, light: Color, text: &str) -> Self {
        Self {
            key,
            group,
            label,
            light,
            text: text.to_owned(),
            expires_at: None,
            days_left: None,
            action: None,
            profile: None,
            id: None,
        }
    }

    /// Adds when it runs out; amber within [`WARN_DAYS`], red once passed.
    fn runs_out(mut self, expires_at: String, days_left: Option<i64>) -> Self {
        if let Some(d) = days_left {
            if d < 0 {
                self.light = Color::Red;
            } else if d <= WARN_DAYS && self.light == Color::Green {
                self.light = Color::Amber;
            }
        }
        self.expires_at = Some(expires_at);
        self.days_left = days_left;
        self
    }

    fn act(mut self, a: Action) -> Self {
        self.action = Some(a);
        self
    }
}

/// The page's whole answer: the rows, and how many need the owner.
#[derive(Debug, Clone, Serialize)]
pub struct Lights {
    pub items: Vec<Light>,
    pub red: usize,
    pub amber: usize,
}

/// The last answer from GitHub, so the page does not ask on every load.
#[derive(Clone, Default)]
pub struct GitHubCache(Arc<Mutex<Option<(Instant, Option<(String, String)>, TokenCheck)>>>);

impl Store {
    /// Whole days from now until `at`, negative once passed. `None` for a
    /// date SQLite cannot read.
    async fn days_until(&self, at: &str) -> Result<Option<i64>> {
        Ok(sqlx::query_scalar(
            // Rounded down, also below zero (SQLite may lack floor()).
            "SELECT CAST(d AS INTEGER) - (d < CAST(d AS INTEGER))
             FROM (SELECT julianday(?1) - julianday('now') AS d) WHERE d IS NOT NULL",
        )
        .bind(at)
        .fetch_optional(self.pool())
        .await?)
    }

    /// `at` plus `days`, in the hub's own date format.
    async fn plus_days(&self, at: &str, days: i64) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', ?1, ?2) WHERE julianday(?1) IS NOT NULL",
        )
        .bind(at)
        .bind(format!("+{days} days"))
        .fetch_optional(self.pool())
        .await?)
    }

    /// GitHub's expiry text ("2026-11-17 00:00:00 UTC" or with an offset
    /// like "-0700") in the hub's own format, in UTC.
    async fn github_date(&self, text: &str) -> Result<Option<String>> {
        let mut parts = text.split_whitespace();
        let (Some(day), Some(time)) = (parts.next(), parts.next()) else {
            return Ok(None);
        };
        let shape = |s: &str, sep: u8, n: usize| {
            s.len() == n
                && s.bytes().enumerate().all(|(i, b)| {
                    b.is_ascii_digit() || (b == sep && (i == 4 || i == 7 || i == 2 || i == 5))
                })
        };
        if !shape(day, b'-', 10) || !shape(time, b':', 8) {
            return Ok(None);
        }
        let offset_minutes = match parts.next() {
            None | Some("UTC") | Some("Z") => 0,
            Some(z) if z.len() == 5 && (z.starts_with('+') || z.starts_with('-')) => {
                let (Ok(h), Ok(m)) = (z[1..3].parse::<i64>(), z[3..5].parse::<i64>()) else {
                    return Ok(None);
                };
                let m = h * 60 + m;
                if z.starts_with('-') {
                    -m
                } else {
                    m
                }
            }
            Some(_) => return Ok(None),
        };
        // UTC is the local time minus its offset.
        Ok(sqlx::query_scalar(
            "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', ?1, ?2) WHERE julianday(?1) IS NOT NULL",
        )
        .bind(format!("{day} {time}"))
        .bind(format!("{:+} minutes", -offset_minutes))
        .fetch_optional(self.pool())
        .await?)
    }

    /// True the first time it is called for this item and date.
    async fn first_reminder(&self, item: &str, expires_at: &str) -> Result<bool> {
        let done = sqlx::query(
            "INSERT INTO sign_in_reminders (item, expires_at) VALUES (?1, ?2)
             ON CONFLICT (item) DO UPDATE SET expires_at = excluded.expires_at,
                 sent_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE sign_in_reminders.expires_at <> excluded.expires_at",
        )
        .bind(item)
        .bind(expires_at)
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() == 1)
    }
}

/// Every sign-in, with its light.
pub async fn lights(st: &AppState) -> Result<Lights> {
    let mut items = Vec::new();
    let apps = st.store.apps().await?;

    for p in [Profile::A, Profile::B] {
        let label = format!("Claude {}", p.as_str());
        let used = apps.iter().any(|a| a.profile == p && !a.pay_per_use);
        let row = match claude_sign_in::saved(st, p).await? {
            None => Light::new(
                format!("claude-{}", p.as_str()),
                "claude",
                label,
                Color::Grey,
                "This hub has no key store, so it can't sign in to Claude.",
            ),
            Some((false, _)) if used => Light::new(
                format!("claude-{}", p.as_str()),
                "claude",
                label.clone(),
                Color::Red,
                &format!("Not signed in. Work on {label} can't start until you sign in."),
            )
            .act(Action::ClaudeSignIn),
            Some((false, _)) => Light::new(
                format!("claude-{}", p.as_str()),
                "claude",
                label,
                Color::Grey,
                "Not signed in. No app uses this plan yet.",
            )
            .act(Action::ClaudeSignIn),
            Some((true, Some(d))) => {
                let days = st.store.days_until(&d.expires_at).await?;
                Light::new(
                    format!("claude-{}", p.as_str()),
                    "claude",
                    label,
                    Color::Green,
                    "Signed in",
                )
                .runs_out(d.expires_at, days)
                .act(Action::ClaudeSignIn)
            }
            Some((true, None)) => Light::new(
                format!("claude-{}", p.as_str()),
                "claude",
                label,
                Color::Green,
                "Signed in. Sign in here once to see when it runs out.",
            )
            .act(Action::ClaudeSignIn),
        };
        items.push(Light {
            profile: Some(p),
            ..row
        });
    }

    items.push(github(st).await?);

    let public = st.public_url();
    if !public.as_deref().is_some_and(|u| u.starts_with("https://")) {
        items.push(
            Light::new(
                "phone-connection".into(),
                "phone",
                "Phone connection".into(),
                Color::Grey,
                "Phones can't reach Decurion yet. Connect once to pair a phone.",
            )
            .act(Action::ConnectPhone),
        );
    }
    for d in st.store.devices().await? {
        let since = d.last_seen.clone().unwrap_or_else(|| d.created_at.clone());
        let text = match (d.has_push, d.last_seen.is_some()) {
            (true, true) => "Signed in · notifications on",
            (false, true) => "Signed in · notifications off",
            (_, false) => "Signed in · not used yet",
        };
        let mut row = Light::new(
            format!("device-{}", d.id),
            "phone",
            d.name.clone(),
            Color::Green,
            text,
        );
        if let Some(until) = st.store.plus_days(&since, DEVICE_DAYS).await? {
            let days = st.store.days_until(&until).await?;
            row = row.runs_out(until, days);
        }
        items.push(Light {
            id: Some(d.id),
            ..row
        });
    }

    let cloud = st.store.cloud_workers().await?;
    if cloud.is_empty() {
        items.push(
            Light::new(
                "cloud".into(),
                "cloud",
                "Cloud worker".into(),
                Color::Grey,
                "None added. Your PC does all the work.",
            )
            .act(Action::AddCloud),
        );
    }
    for (id, name) in cloud {
        let ago = st.seconds_ago(&name);
        let (light, text) = match ago {
            Some(s) if s <= crate::ONLINE_SECS => (Color::Green, "Online"),
            Some(_) => (
                Color::Amber,
                "Offline. Work placed there waits until it is back.",
            ),
            None => (
                Color::Amber,
                "Not heard from since Decurion started. Work placed there waits until it is back.",
            ),
        };
        items.push(Light {
            id: Some(id),
            ..Light::new(format!("worker-{id}"), "cloud", name, light, text)
        });
    }

    for s in st.store.all_site_sign_ins().await? {
        let label = format!("{} ({})", s.label, s.app_name);
        let (light, text) = match s.status {
            SignInStatus::None => (Color::Grey, "Not signed in yet"),
            SignInStatus::Ok => (Color::Green, "Signed in"),
            SignInStatus::Expiring => (Color::Amber, "Runs out soon. Sign in again."),
            SignInStatus::Expired => (Color::Red, "Stopped working. Sign in again."),
        };
        let mut row = Light::new(format!("site-{}", s.id), "site", label, light, text)
            .act(Action::SiteSignIn);
        if let Some(at) = s.expires_at.clone() {
            let days = st.store.days_until(&at).await?;
            row = row.runs_out(at, days);
        }
        items.push(Light {
            id: Some(s.id),
            ..row
        });
    }

    let red = items.iter().filter(|i| i.light == Color::Red).count();
    let amber = items.iter().filter(|i| i.light == Color::Amber).count();
    Ok(Lights { items, red, amber })
}

/// The GitHub row. GitHub is asked again after an hour, or as soon as the
/// saved token changes.
async fn github(st: &AppState) -> Result<Light> {
    let info = st.store.github_token_info().await?;
    let row = |light, text: &str| {
        Light::new("github".into(), "github", "GitHub".into(), light, text).act(Action::GithubToken)
    };
    let Some(gh) = st.github.clone() else {
        return Ok(row(
            Color::Grey,
            "This hub is not set up to send work to GitHub.",
        ));
    };
    let cached = {
        let g = st.github_check.0.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref()
            .filter(|(at, was, _)| at.elapsed() < ASK_GITHUB_EVERY && *was == info)
            .map(|(_, _, c)| c.clone())
    };
    let check = match cached {
        Some(c) => c,
        None => {
            let c = gh.check_token().await;
            *st.github_check.0.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((Instant::now(), info.clone(), c.clone()));
            c
        }
    };
    let saved = info.is_some();
    Ok(match check {
        TokenCheck::NoToken => row(
            Color::Grey,
            "No token. You open each pull request yourself, with one tap.",
        ),
        TokenCheck::Refused if saved => row(
            Color::Red,
            "GitHub no longer takes this token. Save a new one.",
        ),
        TokenCheck::Refused => row(
            Color::Amber,
            "GitHub no longer takes this PC's GitHub sign-in. Save a token here instead.",
        ),
        TokenCheck::Unknown => row(
            Color::Green,
            if saved {
                "Token saved. GitHub could not be asked about it just now."
            } else {
                "Using this PC's GitHub sign-in. GitHub could not be asked about it just now."
            },
        ),
        TokenCheck::Works { expires } => {
            let r = row(
                Color::Green,
                if saved {
                    "Token saved. Decurion opens pull requests for you."
                } else {
                    "Using this PC's GitHub sign-in. Decurion opens pull requests for you."
                },
            );
            match expires {
                Some(text) => match st.store.github_date(&text).await? {
                    Some(at) => {
                        let days = st.store.days_until(&at).await?;
                        r.runs_out(at, days)
                    }
                    None => r,
                },
                None => r,
            }
        }
    })
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/sign-in-lights", get(list))
}

async fn list(State(st): State<AppState>, Extension(c): Extension<Caller>) -> Response {
    if c != Caller::Owner {
        return forbidden("seeing sign-ins");
    }
    match lights(&st).await {
        Ok(l) => Json(l).into_response(),
        Err(e) => ApiError::from(e).into_response(),
    }
}

/// Sends the week-ahead reminders that are due. Returns how many went out.
pub async fn remind(st: &AppState) -> Result<usize> {
    let mut sent = 0;
    for item in lights(st).await?.items {
        let (Some(at), Some(days)) = (item.expires_at.as_deref(), item.days_left) else {
            continue;
        };
        if !(0..=WARN_DAYS).contains(&days) || !st.store.first_reminder(&item.key, at).await? {
            continue;
        }
        sent += 1;
        let Some(p) = st.push.clone() else { continue };
        let when = match days {
            0 => "today".to_owned(),
            1 => "tomorrow".to_owned(),
            n => format!("in {n} days"),
        };
        p.notify_to(
            &st.store,
            "A sign-in runs out soon",
            &format!(
                "{} runs out {when}. Open Settings, Sign-ins to renew it.",
                item.label
            ),
            "/inbox#settings/sign-ins",
        )
        .await;
    }
    Ok(sent)
}

/// Looks for due reminders every hour while the hub runs.
pub fn watch(st: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(REMIND_EVERY);
        loop {
            tick.tick().await;
            if let Err(e) = remind(&st).await {
                tracing::warn!(error = %e, "could not look for sign-ins running out");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn github_dates_are_read_in_utc() {
        let s = Store::open("sqlite::memory:").await.expect("store");
        let d = |t: &'static str| {
            let s = s.clone();
            async move { s.github_date(t).await.expect("query") }
        };
        assert_eq!(
            d("2026-11-17 00:00:00 UTC").await.as_deref(),
            Some("2026-11-17T00:00:00.000Z")
        );
        assert_eq!(
            d("2024-04-04 18:57:29 -0700").await.as_deref(),
            Some("2024-04-05T01:57:29.000Z")
        );
        assert_eq!(
            d("2024-04-04 18:57:29 +0530").await.as_deref(),
            Some("2024-04-04T13:27:29.000Z")
        );
        for bad in [
            "",
            "soon",
            "2024-04-04",
            "2024-04-04 18:57:29 PST",
            "2024/04/04 18:57:29 UTC",
        ] {
            assert_eq!(d(bad).await, None, "{bad}");
        }
    }
}
