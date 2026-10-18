//! Sending accepted work to GitHub, so it is ready for the owner to merge.
//!
//! When the owner accepts a piece of work (or approved the plan of a goal
//! whose piece then passes its check, see [`crate::coordinator`]), the hub
//! pushes it to the app's own `origin` remote as a new branch and, when that remote is on GitHub,
//! opens a pull request for the owner to merge there. Rules that must stay
//! true:
//!
//! - **Only the hub sends**, on the owner's PC, as the owner, from the
//!   app's own folder and with the owner's own git sign-in (Git Credential
//!   Manager on Windows). Boxes never take part and never hold a GitHub key.
//! - **Never merges, never the default branch.** Work goes only to a new
//!   branch under `decurion/` ([`check_branch`]), never to the remote's
//!   default branch or a name like `main`. Pushes are plain, never forced:
//!   a branch already there with other history is left alone and the send
//!   fails with a reason.
//! - **A fix goes forward on its own pull request's branch, nowhere else.**
//!   Work that fixes review comments (started by the owner's tap, see
//!   [`crate::pr_watch`]) goes to the same `decurion/` branch as the pull
//!   request it answers ([`check_fix_branch`]), and only when it builds on
//!   the last commit Decurion pushed there: a plain fast-forward, checked
//!   before the push and refused by git otherwise.
//! - **Only the accepted commit.** The commit is fixed with the accept (the
//!   work's latest saved copy, which cannot change once it is in review),
//!   and a saved copy that holds anything else is not sent.
//! - **Never waits on a question.** git runs with terminal, helper and SSH
//!   prompts off, its input closed, and a time limit.
//! - **No token on a command line or in a log.** The push uses git's own
//!   sign-in; the optional GitHub token (Settings, or the GitHub CLI's own
//!   sign-in) only goes in a request header to GitHub's API. Remote
//!   addresses and git's messages are logged only after [`scrub`] takes any
//!   user name or password out of them.
//! - **Crash safe.** The wish to send is saved in the same transaction as the
//!   accept (`gh_state = 'waiting'`, [`want_send`]). A send marks it
//!   `sending` before acting. A hub that stops halfway finds it on restart
//!   ([`resume`]) and sends again, which is safe: a plain push of the same
//!   commit changes nothing, and an existing pull request is looked for
//!   before a new one is opened.
//! - **A failure never undoes the accept.** The work stays Done, the reason
//!   is saved in plain words for the board, the owner can tap Try again, and
//!   the by-hand route (`<data>/review/<app>-<n>.git`) keeps working.
//!
//! The agent's work is read from the hub's own saved copy (the code part of
//! its latest checkpoint) into the hub's bare copy of the app's repo, with
//! git checking every object. The push runs in the owner's folder with that
//! bare copy lent as extra object storage, so nothing is written into the
//! owner's folder and their own remote settings apply.

use crate::checkpoints::{git, Files};
use crate::netgit;
use crate::push::{Notice, Pusher};
use crate::store::{audit, check_receipt, receipt_result, save_receipt, Result, Store, StoreError};
use crate::{forbidden, ApiError, AppState, Caller};
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use decurion_proto::goal::{Basis, Evidence, ReviewClaim};
use decurion_proto::{GitHubSend, SendState, Thread, ThreadState};
use serde::Deserialize;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, Transaction};
use std::path::Path;
use std::process::Stdio;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

/// Every branch Decurion sends starts with this.
pub const PREFIX: &str = "decurion/";

/// GitHub's own API.
pub const GITHUB_API: &str = "https://api.github.com";

/// Longest branch name sent.
const MAX_BRANCH: usize = 100;

/// Names that are some repo's main line somewhere. Work never goes there,
/// even if a branch name somehow came out as one of them.
const NEVER: &[&str] = &[
    "main",
    "master",
    "trunk",
    "develop",
    "development",
    "dev",
    "staging",
    "production",
    "prod",
    "release",
    "gh-pages",
    "head",
];

/// The actor recorded for the hub's own audit rows.
const ACTOR: &str = "hub";

/// How long one push may take before it is stopped.
const PUSH_SECS: u64 = 180;

const NO_COPY: &str = "Decurion has no saved copy of this work, so it could not send it to GitHub. You can still get it by hand: see Help, Review and merge work.";
const NO_REMOTE: &str = "This app's folder is not linked to GitHub (it has no remote named origin), so the work was not sent. You can still get it by hand: see Help, Review and merge work.";
const NOT_READ: &str = "Decurion could not read this work from its saved copy, so it was not sent. Tap Try again. You can still get it by hand: see Help, Review and merge work.";
const PR_ENDED: &str = "The pull request this fix answers was merged or closed on GitHub, so the fix was not sent. Ask for the change as new work instead.";
const NOT_FORWARD: &str = "This fix does not build on the work already in its pull request, so Decurion did not send it and nothing on GitHub changed. Start the fix again from the comments.";
const NOT_READ_APP: &str = "Decurion could not read the app's folder on this PC, so the work was not sent. Check that the folder is still there, then tap Try again.";

// ---------- Names and addresses ----------

/// The branch accepted work goes to: `decurion/<app>-<n>-<words from the
/// title>`, in lowercase letters, digits and dashes only.
pub fn send_branch(app: &str, thread: i64, title: &str) -> String {
    let app = match slug(app, 30) {
        s if s.is_empty() => "app".to_owned(),
        s => s,
    };
    let words = slug(title, 40);
    if words.is_empty() {
        format!("{PREFIX}{app}-{thread}")
    } else {
        format!("{PREFIX}{app}-{thread}-{words}")
    }
}

/// Lowercase letters and digits, with one dash for anything between them.
fn slug(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if out.len() >= max {
            break;
        }
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

/// Whether work may go to `branch`: under `decurion/`, made only of
/// lowercase letters, digits and dashes, and never the remote's default
/// branch (`default`, when known) or a usual main-line name. The text is
/// for the owner.
pub fn check_branch(branch: &str, default: Option<&str>) -> std::result::Result<(), String> {
    let rest = branch.strip_prefix(PREFIX).unwrap_or("");
    let shaped = !rest.is_empty()
        && branch.len() <= MAX_BRANCH
        && rest
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !rest.starts_with('-')
        && !rest.ends_with('-');
    if !shaped {
        return Err(format!(
            "Decurion only sends work to new branches named like decurion/shop-7-fix-login, and {branch:?} is not one, so nothing was sent."
        ));
    }
    let main_line = default.is_some_and(|d| d.eq_ignore_ascii_case(branch))
        || NEVER.contains(&branch.to_ascii_lowercase().as_str());
    if main_line {
        return Err(
            "Decurion never sends work to the app's main branch: merging there is yours. Nothing was sent."
                .into(),
        );
    }
    Ok(())
}

/// Whether a fix of review comments may go to `branch`: it must be exactly
/// the branch of the pull request it answers (`pr_branch`), and that
/// branch must pass [`check_branch`]. The text is for the owner.
pub fn check_fix_branch(
    branch: &str,
    pr_branch: &str,
    default: Option<&str>,
) -> std::result::Result<(), String> {
    check_branch(branch, default)?;
    if branch != pr_branch {
        return Err(format!(
            "A fix only goes to the branch of the pull request it answers, and {branch:?} is not that branch, so nothing was sent."
        ));
    }
    Ok(())
}

/// Whether `login` looks like a GitHub user name: 1-39 letters, digits and
/// single dashes, not starting or ending with a dash.
pub fn is_login(login: &str) -> bool {
    (1..=39).contains(&login.len())
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !login.starts_with('-')
        && !login.ends_with('-')
        && !login.contains("--")
}

/// The number at the end of a pull request page's address.
pub fn pr_number(url: &str) -> Option<i64> {
    let (_, n) = url.trim_end_matches('/').rsplit_once("/pull/")?;
    n.parse().ok().filter(|n| *n > 0)
}

/// A repo on GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub owner: String,
    pub name: String,
}

/// The GitHub repo a remote address points at, or `None` when it is not on
/// github.com. Takes web (`https://github.com/o/r.git`, with or without a
/// user name), SSH (`git@github.com:o/r.git`, `ssh://git@github.com/o/r`)
/// and `git://` addresses.
pub fn github_repo(url: &str) -> Option<Repo> {
    let url = url.trim();
    let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
        let known = ["https", "http", "ssh", "git", "git+ssh", "ssh+git"];
        if !known.contains(&scheme.to_ascii_lowercase().as_str()) {
            return None;
        }
        let (auth, path) = rest.split_once('/')?;
        let host = auth.rsplit('@').next()?;
        (host.split(':').next()?, path)
    } else {
        // `[user@]host:owner/name`, which git reads as SSH.
        let (auth, path) = url.split_once(':')?;
        if auth.contains(['/', '\\']) {
            return None;
        }
        (auth.rsplit('@').next()?, path)
    };
    if !host.eq_ignore_ascii_case("github.com") && !host.eq_ignore_ascii_case("www.github.com") {
        return None;
    }
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    let owner_ok = (1..=39).contains(&owner.len())
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let name_ok = (1..=100).contains(&name.len())
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    (owner_ok && name_ok).then(|| Repo {
        owner: owner.to_owned(),
        name: name.to_owned(),
    })
}

/// The page on GitHub that opens a pull request for `branch` in one tap,
/// against `base` (GitHub picks the default branch when it is not known).
pub fn compare_url(repo: &Repo, base: Option<&str>, branch: &str) -> String {
    let refs = match base {
        Some(b) => format!("{}...{}", url_part(b), url_part(branch)),
        None => url_part(branch),
    };
    format!(
        "https://github.com/{}/{}/compare/{refs}?expand=1",
        repo.owner, repo.name
    )
}

/// Percent-encodes everything but the characters a branch path needs.
fn url_part(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Takes any user name or password out of the web addresses in `text`, so
/// a remote address or git's own messages can be logged.
pub fn scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| c == '/' || c.is_whitespace() || c == '\'' || c == '"')
            .unwrap_or(tail.len());
        let auth = &tail[..end];
        out.push_str(auth.rfind('@').map_or(auth, |at| &auth[at + 1..]));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

// ---------- The pull request's words ----------

/// What the pull request says about the work.
#[derive(Debug, Default, Clone)]
pub struct Story {
    pub title: String,
    /// The goal this piece belongs to.
    pub goal: Option<String>,
    /// The agent's own final summary.
    pub summary: Option<String>,
    /// The checker's notes, for a piece of a goal.
    pub review: Option<String>,
    /// The hub held it for the owner because it changes protected parts.
    pub held: bool,
    /// The GitHub login who reviews the app's pull requests.
    pub reviewer: Option<String>,
    /// What the checker ran in its last check, and what came of it.
    pub evidence: Vec<Evidence>,
    /// What the checker said about the work, each checked or guessed.
    pub claims: Vec<ReviewClaim>,
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut c: String = s.chars().take(max).collect();
        c.push_str("...");
        c
    }
}

/// One line of a checker's receipt for a bullet: on one line so it cannot
/// break the list, any sign-in taken out of web addresses, and clipped.
fn receipt_line(s: &str, max: usize) -> String {
    let one = s.split_whitespace().collect::<Vec<_>>().join(" ");
    clip(&scrub(&one), max)
}

/// The pull request's title: the work's own title.
pub fn pr_title(s: &Story) -> String {
    clip(&s.title, 120)
}

/// The pull request's text, in plain words for the owner.
pub fn pr_body(s: &Story) -> String {
    let mut b = String::from("## What was done\n\n");
    match s
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        Some(t) => b.push_str(&clip(t, 4000)),
        None => b.push_str("The agent did not leave a summary."),
    }
    b.push_str("\n\n## The checker\n\n");
    match s.review.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => b.push_str(&clip(t, 3000)),
        None => b.push_str("No checker looked at this work: it was added straight to the board."),
    }
    // The checker's receipts, so a reader can tell what was proven by a test
    // or a screen from what the checker only reasoned. The list sizes are
    // already bounded by `ReviewVerdict::validate`; take no more than that.
    let ran: Vec<String> = s
        .evidence
        .iter()
        .take(decurion_proto::goal::MAX_EVIDENCE)
        .map(|e| {
            format!(
                "- {}: {}",
                receipt_line(&e.ran, 300),
                receipt_line(&e.result, 500)
            )
        })
        .collect();
    if !ran.is_empty() {
        b.push_str("\n\nWhat it ran:\n\n");
        b.push_str(&ran.join("\n"));
    }
    let found: Vec<String> = s
        .claims
        .iter()
        .take(decurion_proto::goal::MAX_CLAIMS)
        .map(|c| {
            let how = match c.how {
                Basis::Checked => "Checked",
                Basis::Guessed => "Guessed",
            };
            format!("- {how}: {}", receipt_line(&c.text, 500))
        })
        .collect();
    if !found.is_empty() {
        b.push_str("\n\nWhat it found:\n\n");
        b.push_str(&found.join("\n"));
    }
    if s.held {
        b.push_str(
            "\n\n## Protected parts\n\nDecurion held this work for you because it changes \
protected parts of the app (listed in the checker's notes above). You looked at it and accepted it.",
        );
    }
    b.push_str("\n\n---\n\n");
    if let Some(g) = s.goal.as_deref().map(str::trim).filter(|g| !g.is_empty()) {
        b.push_str(&format!("Part of the goal: {}\n\n", clip(g, 300)));
    }
    if let Some(r) = s.reviewer.as_deref().filter(|r| is_login(r)) {
        b.push_str(&format!("{r} reviews next.\n\n"));
    }
    b.push_str(
        "Decurion made this pull request when you accepted the work. Decurion never merges by \
itself: look it over and merge it here when you are ready.",
    );
    b
}

// ---------- Tokens ----------

/// A GitHub token. Never printed, logged or put on a command line.
pub struct Token(String);

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(<hidden>)")
    }
}

/// Where a token for opening pull requests comes from. Without one, the
/// owner gets a link that opens the pull request in one tap.
pub enum Tokens {
    /// Never a token: always the link.
    None,
    /// This token (tests point the API at a fake).
    Fixed(String),
    /// The owner's token saved in Settings, else the GitHub CLI's own
    /// sign-in (`gh auth token`) when `gh` is installed and signed in.
    /// Neither ever asks the owner anything.
    Owner(Option<decurion_vault::Vault>),
}

impl Tokens {
    pub(crate) async fn get(&self) -> Option<Token> {
        match self {
            Tokens::None => None,
            Tokens::Fixed(t) => Some(Token(t.clone())),
            Tokens::Owner(vault) => {
                if let Some(v) = vault.clone() {
                    if let Ok(Ok(Some(t))) =
                        tokio::task::spawn_blocking(move || v.get_github()).await
                    {
                        let t = t.trim();
                        if !t.is_empty() {
                            return Some(Token(t.to_owned()));
                        }
                    }
                }
                gh_cli_token().await
            }
        }
    }
}

/// The GitHub CLI's own sign-in, if it has one. Prompts and update checks
/// are off, and it gets ten seconds.
async fn gh_cli_token() -> Option<Token> {
    let mut cmd = Command::new("gh");
    cmd.args(["auth", "token", "--hostname", "github.com"])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(10), cmd.output())
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let t = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    ((20..=255).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_graphic())).then_some(Token(t))
}

/// What GitHub said about the token that opens pull requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TokenCheck {
    /// No token saved, and the GitHub CLI is not signed in.
    NoToken,
    /// GitHub takes it. `expires` is GitHub's own date text, when the token
    /// runs out at all.
    Works { expires: Option<String> },
    /// GitHub no longer takes it.
    Refused,
    /// GitHub could not be asked just now.
    Unknown,
}

// ---------- GitHub ----------

/// The hub's way to GitHub: its API address, where tokens come from, and
/// the one-at-a-time lock for sends.
pub struct GitHub {
    api: String,
    http: reqwest::Client,
    pub(crate) tokens: Tokens,
    sending: tokio::sync::Mutex<()>,
}

/// A pull request on GitHub: its page and its number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pr {
    pub url: String,
    pub number: i64,
}

impl Pr {
    fn from_json(v: &serde_json::Value) -> Option<Self> {
        let url = v["html_url"].as_str().filter(|u| pr_url_ok(u))?;
        let number = v["number"]
            .as_i64()
            .filter(|n| *n > 0)
            .or_else(|| pr_number(url))?;
        Some(Self {
            url: url.to_owned(),
            number,
        })
    }
}

impl GitHub {
    /// `api` is GitHub's API ([`GITHUB_API`]), or a fake one in tests.
    pub fn new(api: &str, tokens: Tokens) -> reqwest::Result<Self> {
        // An API that hangs or redirects must not hold up sends.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("decurion")
            .build()?;
        Ok(Self {
            api: api.trim_end_matches('/').to_owned(),
            http,
            tokens,
            sending: tokio::sync::Mutex::new(()),
        })
    }

    pub(crate) fn req(
        &self,
        method: reqwest::Method,
        path: &str,
        t: &Token,
    ) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.api))
            .bearer_auth(&t.0)
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
    }

    /// Asks GitHub about the token that opens pull requests, for the
    /// Sign-ins page: whether GitHub still takes it and when it runs out
    /// (GitHub says so in a header for tokens that expire). The token
    /// itself never leaves this function.
    pub(crate) async fn check_token(&self) -> TokenCheck {
        let Some(t) = self.tokens.get().await else {
            return TokenCheck::NoToken;
        };
        let Ok(resp) = self.req(reqwest::Method::GET, "/user", &t).send().await else {
            return TokenCheck::Unknown;
        };
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return TokenCheck::Refused;
        }
        if !resp.status().is_success() {
            return TokenCheck::Unknown;
        }
        TokenCheck::Works {
            expires: resp
                .headers()
                .get("github-authentication-token-expiration")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
        }
    }

    /// A pull request already open (or closed) for `branch`, so a resend
    /// never opens a second one.
    async fn find_pr(
        &self,
        t: &Token,
        r: &Repo,
        branch: &str,
    ) -> std::result::Result<Option<Pr>, String> {
        let head = format!("{}:{branch}", r.owner);
        let resp = self
            .req(
                reqwest::Method::GET,
                &format!("/repos/{}/{}/pulls", r.owner, r.name),
                t,
            )
            .query(&[("head", head.as_str()), ("state", "all"), ("per_page", "5")])
            .send()
            .await
            .map_err(|_| unreachable_api())?;
        if !resp.status().is_success() {
            return Err(api_refused(resp.status()));
        }
        let list: Vec<serde_json::Value> = resp.json().await.map_err(|_| unreadable_api())?;
        Ok(list.iter().find_map(Pr::from_json))
    }

    async fn default_branch(&self, t: &Token, r: &Repo) -> std::result::Result<String, String> {
        let resp = self
            .req(
                reqwest::Method::GET,
                &format!("/repos/{}/{}", r.owner, r.name),
                t,
            )
            .send()
            .await
            .map_err(|_| unreachable_api())?;
        if !resp.status().is_success() {
            return Err(api_refused(resp.status()));
        }
        let v: serde_json::Value = resp.json().await.map_err(|_| unreadable_api())?;
        v["default_branch"]
            .as_str()
            .filter(|b| !b.is_empty())
            .map(str::to_owned)
            .ok_or_else(unreadable_api)
    }

    async fn open_pr(
        &self,
        t: &Token,
        r: &Repo,
        branch: &str,
        base: &str,
        s: &Story,
    ) -> std::result::Result<Pr, String> {
        let resp = self
            .req(
                reqwest::Method::POST,
                &format!("/repos/{}/{}/pulls", r.owner, r.name),
                t,
            )
            .json(&serde_json::json!({
                "title": pr_title(s),
                "head": branch,
                "base": base,
                "body": pr_body(s),
                "maintainer_can_modify": false,
            }))
            .send()
            .await
            .map_err(|_| unreachable_api())?;
        if !resp.status().is_success() {
            return Err(api_refused(resp.status()));
        }
        let v: serde_json::Value = resp.json().await.map_err(|_| unreadable_api())?;
        Pr::from_json(&v).ok_or_else(unreadable_api)
    }

    /// Asks `login` to review pull request `number`. Asking again is
    /// harmless, so a resend or a fix asks again.
    async fn request_review(
        &self,
        t: &Token,
        r: &Repo,
        number: i64,
        login: &str,
    ) -> std::result::Result<(), String> {
        let resp = self
            .req(
                reqwest::Method::POST,
                &format!("/repos/{}/{}/pulls/{number}/requested_reviewers", r.owner, r.name),
                t,
            )
            .json(&serde_json::json!({ "reviewers": [login] }))
            .send()
            .await
            .map_err(|_| {
                format!("Could not reach GitHub to ask {login} to review. Ask them on the pull request.")
            })?;
        if resp.status().is_success() {
            return Ok(());
        }
        Err(format!(
            "GitHub would not ask {login} to review (it said {}). They may not have access to this repo, or they opened the pull request. Ask them on the pull request.",
            resp.status().as_u16()
        ))
    }

    /// Finds or opens the pull request for `branch`. Never on the default
    /// branch, and never two.
    async fn pull_request(
        &self,
        t: &Token,
        r: &Repo,
        branch: &str,
        s: &Story,
    ) -> std::result::Result<Pr, String> {
        if let Some(pr) = self.find_pr(t, r, branch).await? {
            return Ok(pr);
        }
        let base = self.default_branch(t, r).await?;
        check_branch(branch, Some(&base))?;
        match self.open_pr(t, r, branch, &base, s).await {
            Ok(pr) => Ok(pr),
            // It may have been opened after all (an answer lost on the way).
            Err(why) => match self.find_pr(t, r, branch).await {
                Ok(Some(pr)) => Ok(pr),
                _ => Err(why),
            },
        }
    }
}

/// Only pull request pages on GitHub are shown and linked.
fn pr_url_ok(u: &str) -> bool {
    u.starts_with("https://github.com/") && !u.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn unreachable_api() -> String {
    "Could not reach GitHub to open the pull request. Open it with the link instead.".into()
}

fn unreadable_api() -> String {
    "GitHub's answer could not be read, so no pull request was opened. Open it with the link instead.".into()
}

fn api_refused(status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        401 => "GitHub did not accept the saved token, so no pull request was opened. Open it with the link instead, or save a new token in Settings.".into(),
        403 | 404 => "The saved GitHub token may not open pull requests on this repo, so none was opened. Open it with the link instead, or give the token Pull requests: Read and write for this repo.".into(),
        _ => format!("GitHub said {} when asked to open the pull request. Open it with the link instead.", status.as_u16()),
    }
}

// ---------- The push ----------

/// git's arguments for the push, after what [`netgit::command`] puts first
/// (`-C <app folder>`, hooks and file watchers off). Pure, so a test
/// can check that nothing secret is ever among them: only the commit and
/// the branch, to the remote by its name.
pub fn push_args(head: &str, branch: &str) -> Vec<String> {
    [
        "-c",
        "push.followTags=false",
        "-c",
        "push.recurseSubmodules=no",
        "-c",
        "push.gpgSign=false",
        "-c",
        "push.negotiate=false",
        "-c",
        "remote.origin.mirror=false",
        "push",
        "--no-verify",
        "--quiet",
        "origin",
        &format!("{head}:refs/heads/{branch}"),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Pushes `head` from the owner's folder `repo` to `branch` on its
/// `origin`, borrowing the objects in the hub's bare copy `cache`. A plain
/// push: never forced, so other history on that branch is left alone.
async fn push(
    repo: &Path,
    cache: &Path,
    head: &str,
    branch: &str,
    place: &str,
) -> std::result::Result<(), String> {
    let mut cmd = netgit::command(repo).await;
    cmd.args(push_args(head, branch))
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", cache.join("objects"));
    let out = match netgit::output(cmd, Duration::from_secs(PUSH_SECS)).await {
        Err(netgit::Failed::TooSlow) => {
            return Err(format!(
                "{place} took too long to answer, so the work was not sent. Tap Try again."
            ))
        }
        Err(netgit::Failed::NotStarted(e)) => {
            tracing::warn!(error = %e, "could not start git to push");
            return Err(
                "Decurion could not start git on this PC, so the work was not sent. Tap Try again."
                    .into(),
            );
        }
        Ok(out) => out,
    };
    if out.status.success() {
        return Ok(());
    }
    let err = scrub(&String::from_utf8_lossy(&out.stderr));
    tracing::warn!(branch, error = %err.trim(), "push to the app's remote failed");
    Err(push_failure(&err, branch, place))
}

/// Plain words for a failed push, from git's own message.
fn push_failure(stderr: &str, branch: &str, place: &str) -> String {
    let e = stderr.to_ascii_lowercase();
    let any = |words: &[&str]| words.iter().any(|w| e.contains(w));
    if any(&["non-fast-forward", "fetch first", "[rejected]"]) {
        format!("A branch named {branch} is already on {place} with other changes, so Decurion did not overwrite it. Delete or rename that branch there, then tap Try again.")
    } else if e.contains("[remote rejected]") {
        format!("{place} refused the new branch {branch}. The repo may have rules about branch names. You can still get the work by hand: see Help, Review and merge work.")
    } else if any(&[
        "authentication",
        "could not read username",
        "could not read password",
        "terminal prompts disabled",
        "permission denied",
        "permission to",
        "403",
        "401",
        "access denied",
        "not authorized",
        "invalid username or password",
        "repository not found",
        "host key verification failed",
    ]) {
        format!("This PC is not signed in to {place} with access to this app's repo, so the work was not sent. Push once from this PC yourself (for example with GitHub Desktop) so it remembers you, then tap Try again.")
    } else if any(&[
        "could not resolve",
        "unable to access",
        "connection",
        "timed out",
        "network",
        "proxy",
    ]) {
        format!("Could not reach {place}. Check this PC's internet connection, then tap Try again.")
    } else {
        format!("Sending to {place} did not work. Tap Try again. If it keeps failing, you can still get the work by hand: see Help, Review and merge work.")
    }
}

/// A branch name safe to put in a refspec (the work's own branch, as the
/// runner saved it).
pub(crate) fn plain_branch(b: &str) -> bool {
    !b.is_empty()
        && !b.starts_with(['-', '/'])
        && !b.ends_with(['/', '.'])
        && !b.contains("..")
        && !b.contains("//")
        && b.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'/' | b'-' | b'_' | b'.'))
}

// ---------- Store ----------

/// What a send needs to know about a thread.
struct Job {
    app_id: i64,
    repo_path: String,
    branch: String,
    /// The commit accepted, recorded with the accept.
    accepted: Option<String>,
    work_branch: String,
    checkpoint: Option<i64>,
    story: Story,
    /// For a fix of review comments: the pull request it answers.
    fix: Option<FixOf>,
}

/// The watched pull request a fix goes to.
struct FixOf {
    /// The work that opened the pull request.
    watch: i64,
    branch: String,
    /// The last commit Decurion pushed there. The fix must build on it.
    head: String,
    url: String,
    number: i64,
    open: bool,
}

/// What was sent.
struct Sent {
    url: Option<String>,
    pull_request: bool,
    note: Option<String>,
    /// The commit now on the branch.
    head: Option<String>,
    /// The pull request's repo and number, to watch it.
    watch: Option<(Repo, i64)>,
}

impl Sent {
    fn plain(url: Option<String>, head: &str) -> Self {
        Self {
            url,
            pull_request: false,
            note: None,
            head: Some(head.to_owned()),
            watch: None,
        }
    }
}

/// Where the hub's bare copy keeps the last commit pushed for a pull
/// request, so a fix can start from it and be checked against it.
fn pr_ref(watch: i64) -> String {
    format!("refs/decurion/pr/{watch}")
}

/// The board's view of a thread's send, from its `gh_*` columns.
pub(crate) fn send_from(row: &SqliteRow) -> Result<Option<GitHubSend>> {
    let Some(state) = row.try_get::<Option<String>, _>("gh_state")? else {
        return Ok(None);
    };
    Ok(Some(GitHubSend {
        state: SendState::from_str(&state).map_err(StoreError::Invalid)?,
        branch: row
            .try_get::<Option<String>, _>("gh_branch")?
            .unwrap_or_default(),
        url: row.try_get("gh_url")?,
        pull_request: row.try_get("gh_pr")?,
        error: row.try_get("gh_error")?,
    }))
}

/// Records, inside the accept's own transaction, that the hub is to send
/// thread `id` to GitHub, and the branch it goes to. Work sent before is
/// left as it is. A research piece changed no code, so nothing of it is
/// ever sent: accepting it only marks it done. A fix of review comments
/// goes to the branch of the pull request it answers instead of a new one.
pub(crate) async fn want_send(tx: &mut Transaction<'_, Sqlite>, id: i64) -> Result<()> {
    let (app, title, research, pr_branch): (String, String, bool, Option<String>) = sqlx::query_as(
        "SELECT a.name, t.title, t.research, w.branch FROM threads t JOIN apps a ON a.id = t.app_id
         LEFT JOIN pr_watches w ON w.thread_id = t.fix_of
         WHERE t.id = ?",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    if research {
        return Ok(());
    }
    let branch = pr_branch.unwrap_or_else(|| send_branch(&app, id, &title));
    // The commit sent is the one accepted: the work's latest saved copy,
    // which cannot change once the work is in review.
    sqlx::query(
        "UPDATE threads SET gh_state = 'waiting', gh_branch = ?, gh_error = NULL,
             gh_head = (SELECT c.head FROM checkpoints c WHERE c.thread_id = threads.id
                        ORDER BY c.id DESC LIMIT 1)
         WHERE id = ? AND gh_state IS NULL",
    )
    .bind(branch)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

impl Store {
    async fn send_job(&self, id: i64) -> Result<Job> {
        let row = sqlx::query(
            "SELECT t.app_id, a.repo_path, t.title, t.gh_branch, t.gh_head, t.branch,
                 (SELECT MAX(c.id) FROM checkpoints c WHERE c.thread_id = t.id) AS checkpoint,
                 p.review, p.review_summary, p.review_evidence, p.review_claims,
                 g.title AS goal, a.reviewer,
                 w.thread_id AS watch, w.branch AS w_branch, w.head AS w_head, w.url AS w_url,
                 w.number AS w_number, w.state AS w_state
             FROM threads t JOIN apps a ON a.id = t.app_id
             LEFT JOIN goal_pieces p ON p.thread_id = t.id
             LEFT JOIN goals g ON g.id = p.goal_id
             LEFT JOIN pr_watches w ON w.thread_id = t.fix_of
             WHERE t.id = ?",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(StoreError::NotFound)?;
        let summary: Option<String> = sqlx::query_scalar(
            "SELECT json_extract(body, '$.summary') FROM events WHERE thread_id = ?
                 AND json_extract(body, '$.kind') = 'finished' AND json_extract(body, '$.ok') = 1
             ORDER BY id DESC LIMIT 1",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?
        .flatten();
        let review: Option<String> = row.try_get("review")?;
        // Work added straight to the board has no piece, so no receipts; a
        // list that does not read only leaves the receipts out.
        let evidence: Vec<Evidence> = row
            .try_get::<Option<String>, _>("review_evidence")?
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        let claims: Vec<ReviewClaim> = row
            .try_get::<Option<String>, _>("review_claims")?
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        let review_summary: Option<String> = row.try_get("review_summary")?;
        let held = crate::protected::is_held(review.as_deref(), review_summary.as_deref());
        Ok(Job {
            app_id: row.try_get("app_id")?,
            repo_path: row.try_get("repo_path")?,
            branch: row
                .try_get::<Option<String>, _>("gh_branch")?
                .unwrap_or_default(),
            accepted: row.try_get("gh_head")?,
            // The name the runner saved the work under, as the checker read it.
            work_branch: row
                .try_get::<Option<String>, _>("branch")?
                .unwrap_or_else(|| format!("decurion/thread-{id}")),
            checkpoint: row.try_get("checkpoint")?,
            story: Story {
                title: row.try_get("title")?,
                goal: row.try_get("goal")?,
                summary,
                review: review_summary,
                held,
                reviewer: row.try_get("reviewer")?,
                evidence,
                claims,
            },
            fix: match row.try_get::<Option<i64>, _>("watch")? {
                None => None,
                Some(watch) => Some(FixOf {
                    watch,
                    branch: row.try_get("w_branch")?,
                    head: row.try_get("w_head")?,
                    url: row.try_get("w_url")?,
                    number: row.try_get("w_number")?,
                    open: row.try_get::<String, _>("w_state")? == "open",
                }),
            },
        })
    }

    /// Takes a waiting send for this hub. False if it is not waiting (sent,
    /// being sent, or never asked for). Research is never sent, whatever
    /// its row says.
    async fn start_send(&self, id: i64) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE threads SET gh_state = 'sending'
             WHERE id = ? AND gh_state = 'waiting' AND state = 'done' AND research = 0",
        )
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() == 1)
    }

    async fn set_send_head(&self, id: i64, head: &str) -> Result<()> {
        sqlx::query(
            "UPDATE threads SET gh_head = ? WHERE id = ? AND gh_state = 'sending' AND gh_head IS NULL",
        )
            .bind(head)
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Ends a send, with its audit row, in one transaction.
    async fn end_send(
        &self,
        id: i64,
        state: SendState,
        sent: Option<&Sent>,
        error: Option<&str>,
        note: &str,
    ) -> Result<()> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let done = sqlx::query(
            "UPDATE threads SET gh_state = ?, gh_url = ?, gh_pr = ?, gh_error = ?
             WHERE id = ? AND gh_state = 'sending'",
        )
        .bind(state.as_str())
        .bind(sent.and_then(|s| s.url.as_deref()))
        .bind(sent.is_some_and(|s| s.pull_request))
        .bind(error)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if done.rows_affected() == 1 {
            audit(
                &mut tx,
                id,
                ACTOR,
                ThreadState::Done,
                ThreadState::Done,
                Some(note),
            )
            .await?;
            if let Some(sent) = sent {
                crate::pr_watch::sent(
                    &mut tx,
                    id,
                    sent.watch.as_ref(),
                    sent.url.as_deref(),
                    sent.head.as_deref(),
                )
                .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// The owner's Try again on a send that failed. With a receipt already
    /// used for this, nothing happens and the thread is returned as it is
    /// now; the flag is false then.
    pub async fn retry_send(&self, id: i64, receipt: Option<&str>) -> Result<(Thread, bool)> {
        check_receipt(receipt)?;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(done) = receipt_result(&mut tx, receipt, "github_send").await? {
            if done != id {
                return Err(StoreError::Duplicate(crate::store::RECEIPT_REUSED.into()));
            }
            tx.commit().await?;
            return Ok((self.thread(id).await?, false));
        }
        let row: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT state, gh_state FROM threads WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let (state, gh) = row.ok_or(StoreError::NotFound)?;
        if state != "done" || gh.as_deref() != Some("failed") {
            return Err(StoreError::Refused(
                "This work is not waiting to be sent to GitHub again.".into(),
            ));
        }
        sqlx::query("UPDATE threads SET gh_state = 'waiting', gh_error = NULL WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        audit(
            &mut tx,
            id,
            crate::store::OWNER,
            ThreadState::Done,
            ThreadState::Done,
            Some("asked to send it to GitHub again"),
        )
        .await?;
        save_receipt(&mut tx, receipt, "github_send", id).await?;
        tx.commit().await?;
        Ok((self.thread(id).await?, true))
    }

    /// After a restart: a send cut off halfway waits again, and every
    /// waiting send is returned, oldest first.
    async fn requeue_sends(&self) -> Result<Vec<i64>> {
        sqlx::query("UPDATE threads SET gh_state = 'waiting' WHERE gh_state = 'sending'")
            .execute(self.pool())
            .await?;
        Ok(sqlx::query_scalar(
            "SELECT id FROM threads WHERE gh_state = 'waiting' AND state = 'done' ORDER BY id",
        )
        .fetch_all(self.pool())
        .await?)
    }

    /// The owner's saved GitHub token, as the board shows it.
    pub(crate) async fn github_token_info(&self) -> Result<Option<(String, String)>> {
        Ok(
            sqlx::query_as("SELECT last_four, added_at FROM owner_keys WHERE name = 'github'")
                .fetch_optional(self.pool())
                .await?,
        )
    }

    async fn save_github_token(&self, last_four: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO owner_keys (name, last_four) VALUES ('github', ?)
             ON CONFLICT (name) DO UPDATE SET last_four = excluded.last_four,
                 added_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        )
        .bind(last_four)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    async fn remove_github_token(&self) -> Result<()> {
        sqlx::query("DELETE FROM owner_keys WHERE name = 'github'")
            .execute(self.pool())
            .await?;
        Ok(())
    }
}

// ---------- Sending ----------

/// Everything a send needs from the hub.
#[derive(Clone)]
pub struct Sender {
    pub(crate) store: Store,
    pub(crate) files: Arc<Files>,
    pub(crate) gh: Arc<GitHub>,
    pub(crate) push: Option<Pusher>,
}

impl Sender {
    /// Sends thread `id` in the background, if it is waiting to be sent.
    pub fn spawn(&self, id: i64) {
        let me = self.clone();
        tokio::spawn(async move { me.send(id).await });
    }

    /// Sends thread `id` if it is waiting to be sent, one send at a time.
    /// Never fails the caller: a failure is saved on the thread.
    pub async fn send(&self, id: i64) {
        let _one = self.gh.sending.lock().await;
        match self.store.start_send(id).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                tracing::error!(error = %e, thread = id, "could not start sending to GitHub");
                return;
            }
        }
        let (title, app) = match self.store.thread(id).await {
            Ok(t) => {
                let app = self.store.app(t.app_id).await.map(|a| a.name);
                (t.title, app.unwrap_or_default())
            }
            Err(_) => Default::default(),
        };
        let (ended, head, url) = match self.attempt(id).await {
            Ok(sent) => {
                let note = match (&sent.url, sent.pull_request) {
                    (Some(u), true) => format!("sent to GitHub, pull request {u}"),
                    (Some(u), false) => format!("sent to GitHub; link to open a pull request: {u}"),
                    (None, _) => "sent to the app's git server as a branch".to_owned(),
                };
                let ended = self
                    .store
                    .end_send(
                        id,
                        SendState::Sent,
                        Some(&sent),
                        sent.note.as_deref(),
                        &note,
                    )
                    .await;
                let head = if sent.url.is_some() {
                    "ready to merge on GitHub"
                } else {
                    "sent as a branch"
                };
                (ended, head, sent.url.unwrap_or_else(|| "/inbox".into()))
            }
            Err(why) => {
                let note = format!("could not send to GitHub: {why}");
                let ended = self
                    .store
                    .end_send(id, SendState::Failed, None, Some(&why), &note)
                    .await;
                (ended, "could not send to GitHub", "/inbox".to_owned())
            }
        };
        if let Err(e) = ended {
            tracing::error!(error = %e, thread = id, "could not record the GitHub send");
            return;
        }
        if let Some(p) = &self.push {
            let n = Notice::work(&app, id, head, &title).open_pr(&url);
            p.send(&self.store, &n).await;
        }
    }

    async fn attempt(&self, id: i64) -> std::result::Result<Sent, String> {
        let job = self.store.send_job(id).await.map_err(|e| {
            tracing::error!(error = %e, thread = id, "could not read the work to send");
            NOT_READ.to_owned()
        })?;
        let checkpoint = job.checkpoint.ok_or_else(|| NO_COPY.to_owned())?;
        if !plain_branch(&job.work_branch) {
            return Err(NOT_READ.to_owned());
        }
        if job.fix.as_ref().is_some_and(|f| !f.open) {
            return Err(PR_ENDED.to_owned());
        }
        let repo_path = Path::new(&job.repo_path);
        let remote = git(repo_path, &["remote", "get-url", "origin"])
            .await
            .map_err(|_| NO_REMOTE.to_owned())?;
        let repo = github_repo(&remote);
        let place = if repo.is_some() {
            "GitHub"
        } else {
            "the app's git server"
        };
        // The default branch as the owner's folder knows it, without asking
        // the network.
        let default = git(
            repo_path,
            &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        )
        .await
        .ok()
        .and_then(|r| r.strip_prefix("refs/remotes/origin/").map(str::to_owned));
        match &job.fix {
            Some(f) => check_fix_branch(&job.branch, &f.branch, default.as_deref())?,
            None => check_branch(&job.branch, default.as_deref())?,
        }
        tracing::info!(thread = id, remote = %scrub(&remote), branch = %job.branch, "sending accepted work");
        let into = format!("refs/decurion/send/{id}");
        let head = {
            let _held = self.files.git_lock.lock().await;
            let cache = self.files.app_cache(job.app_id);
            self.files
                .fetch_app(&cache, &job.repo_path)
                .await
                .map_err(|e| {
                    tracing::warn!(error = %scrub(&e.to_string()), thread = id, "could not read the app's folder");
                    NOT_READ_APP.to_owned()
                })?;
            let head = self
                .files
                .fetch_saved_code(&cache, id, checkpoint, &job.work_branch, &into)
                .await
                .map_err(|e| {
                    tracing::warn!(error = %e, thread = id, "could not read the saved copy");
                    NOT_READ.to_owned()
                })?;
            // Only ever the commit the owner accepted.
            match &job.accepted {
                Some(want) if *want != head => {
                    let _ = git(&cache, &["update-ref", "-d", &into]).await;
                    tracing::error!(thread = id, %want, got = %head, "the saved copy is not the accepted commit");
                    return Err(NOT_READ.to_owned());
                }
                Some(_) => {}
                None => {
                    if let Err(e) = self.store.set_send_head(id, &head).await {
                        tracing::error!(error = %e, thread = id, "could not record the commit to send");
                        return Err(NOT_READ.to_owned());
                    }
                }
            }
            // A fix only moves its pull request's branch forward from the
            // last commit Decurion pushed there, never sideways or back.
            if let Some(f) = &job.fix {
                let forward = f.head != head
                    && git(&cache, &["merge-base", "--is-ancestor", &f.head, &head])
                        .await
                        .is_ok();
                if !forward {
                    let _ = git(&cache, &["update-ref", "-d", &into]).await;
                    tracing::warn!(thread = id, base = %f.head, got = %head, "the fix does not build on the pull request");
                    return Err(NOT_FORWARD.to_owned());
                }
            }
            let pushed = push(repo_path, &cache, &head, &job.branch, place).await;
            if pushed.is_ok() {
                // Kept, so a fix can later start from it and be checked
                // against it.
                let watch = job.fix.as_ref().map_or(id, |f| f.watch);
                let _ = git(&cache, &["update-ref", &pr_ref(watch), &head]).await;
            }
            let _ = git(&cache, &["update-ref", "-d", &into]).await;
            pushed?;
            head
        };
        let Some(repo) = repo else {
            return Ok(Sent::plain(None, &head));
        };
        let token = self.gh.tokens.get().await;
        if let Some(f) = &job.fix {
            // The pull request is already there; its reviewer is asked to
            // look again.
            let note = match (&token, job.story.reviewer.as_deref()) {
                (Some(t), Some(login)) if is_login(login) => self
                    .gh
                    .request_review(t, &repo, f.number, login)
                    .await
                    .err(),
                _ => None,
            };
            return Ok(Sent {
                url: Some(f.url.clone()),
                pull_request: true,
                note,
                head: Some(head),
                watch: Some((repo, f.number)),
            });
        }
        let link = compare_url(&repo, default.as_deref(), &job.branch);
        let Some(token) = token else {
            return Ok(Sent::plain(Some(link), &head));
        };
        Ok(
            match self
                .gh
                .pull_request(&token, &repo, &job.branch, &job.story)
                .await
            {
                Ok(pr) => {
                    let note = match job.story.reviewer.as_deref() {
                        Some(login) if is_login(login) => self
                            .gh
                            .request_review(&token, &repo, pr.number, login)
                            .await
                            .err(),
                        _ => None,
                    };
                    if let Some(n) = &note {
                        tracing::warn!(thread = id, reason = %n, "could not ask for a review");
                    }
                    Sent {
                        url: Some(pr.url),
                        pull_request: true,
                        note,
                        head: Some(head),
                        watch: Some((repo, pr.number)),
                    }
                }
                Err(why) => {
                    tracing::warn!(thread = id, reason = %why, "no pull request opened; giving the link");
                    Sent {
                        note: Some(why),
                        ..Sent::plain(Some(link), &head)
                    }
                }
            },
        )
    }
}

/// Sends whatever was waiting, or cut off halfway, when the hub last
/// stopped. Call once at start. Returns how many it tried.
pub async fn resume(state: &AppState) -> usize {
    let Some(s) = state.sender() else { return 0 };
    let ids = {
        // No send runs while the cut-off ones are put back.
        let _one = s.gh.sending.lock().await;
        match s.store.requeue_sends().await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::error!(error = %e, "could not look for work waiting to go to GitHub");
                return 0;
            }
        }
    };
    for id in &ids {
        s.send(*id).await;
    }
    ids.len()
}

// ---------- Routes ----------

#[derive(Deserialize)]
pub(crate) struct Retry {
    #[serde(default)]
    receipt: Option<String>,
}

/// The board's Try again on accepted work that did not reach GitHub.
pub(crate) async fn retry(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    UrlPath(id): UrlPath<i64>,
    Json(b): Json<Retry>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("sending work to GitHub");
    }
    let Some(sender) = st.sender() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "this hub is not set up to send work to GitHub",
        )
            .into_response();
    };
    match st.store.retry_send(id, b.receipt.as_deref()).await {
        Ok((t, fresh)) => {
            if fresh {
                sender.spawn(id);
            }
            Json(t).into_response()
        }
        Err(e) => ApiError(e).into_response(),
    }
}

/// What Settings shows about the GitHub token: only its last four.
pub(crate) async fn settings(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("seeing the GitHub settings");
    }
    match st.store.github_token_info().await {
        Ok(t) => Json(serde_json::json!({
            "token": t.map(|(last_four, added_at)| serde_json::json!({
                "last_four": last_four,
                "added_at": added_at,
            })),
        }))
        .into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// Settings' "save a GitHub token" form. The body is parsed here rather
/// than by an extractor, so no error message can ever repeat the value.
pub(crate) async fn save_token(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    body: axum::body::Bytes,
) -> Response {
    if c != Caller::Owner {
        return forbidden("saving a GitHub token");
    }
    let Some(vault) = st.vault.clone() else {
        return crate::no_vault();
    };
    let Some(value) = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v["value"].as_str().map(|s| s.trim().to_owned()))
    else {
        return (StatusCode::BAD_REQUEST, "send the token as value").into_response();
    };
    if let Err(e) = decurion_vault::check_github_token(&value) {
        return token_error(e);
    }
    // The vault first: a crash before the row is written leaves at worst an
    // unlisted token, never a listed one with nothing behind it.
    let last_four = decurion_vault::last_four(&value);
    match tokio::task::spawn_blocking(move || vault.put_github(&value)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return token_error(e),
        Err(e) => {
            tracing::error!(error = %e, "key store task failed");
            return crate::key_store_failed();
        }
    }
    match st.store.save_github_token(&last_four).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// Forgets the GitHub token: the value first, then the row.
pub(crate) async fn remove_token(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("removing the GitHub token");
    }
    let Some(vault) = st.vault.clone() else {
        return crate::no_vault();
    };
    match tokio::task::spawn_blocking(move || vault.delete_github()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return token_error(e),
        Err(e) => {
            tracing::error!(error = %e, "key store task failed");
            return crate::key_store_failed();
        }
    }
    match st.store.remove_github_token().await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

fn token_error(e: decurion_vault::VaultError) -> Response {
    match e {
        decurion_vault::VaultError::Invalid(why) => (StatusCode::BAD_REQUEST, why).into_response(),
        decurion_vault::VaultError::Store(why) => {
            tracing::error!(error = %why, "key store error");
            crate::key_store_failed()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branches_are_named_from_the_app_the_number_and_the_title() {
        assert_eq!(
            send_branch("shop", 7, "Fix the login page!"),
            "decurion/shop-7-fix-the-login-page"
        );
        assert_eq!(send_branch("My_App", 12, "   "), "decurion/my-app-12");
        assert_eq!(
            send_branch("__", 3, "Ünïcode only"),
            "decurion/app-3-n-code-only"
        );
        let long = send_branch(&"a".repeat(40), 123456, &"word ".repeat(40));
        assert!(long.len() <= MAX_BRANCH, "{long}");
        for b in [
            send_branch("shop", 7, "Fix the login page!"),
            send_branch("x", 1, "--- ... ---"),
            long,
        ] {
            assert!(check_branch(&b, Some("main")).is_ok(), "{b}");
        }
    }

    #[test]
    fn only_new_decurion_branches_are_ever_sent_to() {
        for bad in [
            "main",
            "master",
            "refs/heads/main",
            "decurion/",
            "decurion/-x",
            "decurion/x-",
            "decurion/Shop-7",
            "decurion/a/b",
            "decurion/a..b",
            "decurion/a b",
            "decurion/a:main",
            "decurion/a@{1}",
            "-decurion/a",
            "feature/x",
            "",
        ] {
            assert!(check_branch(bad, Some("main")).is_err(), "{bad:?}");
        }
        assert!(check_branch(&format!("decurion/{}", "a".repeat(100)), None).is_err());
        // Even a default branch that looks like one of ours is never touched.
        assert!(check_branch("decurion/shop-7", Some("decurion/shop-7")).is_err());
        assert!(check_branch("decurion/shop-7", Some("main")).is_ok());
    }

    #[test]
    fn logins_and_pull_request_numbers_are_read_strictly() {
        for ok in ["param", "Param-K", "a", &"a".repeat(39)] {
            assert!(is_login(ok), "{ok}");
        }
        for bad in ["", "-a", "a-", "a--b", "a b", "a_b", &"a".repeat(40), "a@b"] {
            assert!(!is_login(bad), "{bad}");
        }
        assert_eq!(pr_number("https://github.com/acme/shop/pull/42"), Some(42));
        assert_eq!(pr_number("https://github.com/acme/shop/pull/42/"), Some(42));
        assert_eq!(pr_number("https://github.com/acme/shop/compare/x"), None);
        assert_eq!(pr_number("https://github.com/acme/shop/pull/0"), None);
    }

    #[test]
    fn github_remotes_are_read_in_every_shape() {
        let r = |o: &str, n: &str| {
            Some(Repo {
                owner: o.into(),
                name: n.into(),
            })
        };
        assert_eq!(
            github_repo("https://github.com/acme/shop.git"),
            r("acme", "shop")
        );
        assert_eq!(
            github_repo("https://github.com/acme/shop"),
            r("acme", "shop")
        );
        assert_eq!(
            github_repo("https://github.com/acme/shop/"),
            r("acme", "shop")
        );
        assert_eq!(
            github_repo("https://someone:ghp_secret@github.com/acme/shop.git"),
            r("acme", "shop")
        );
        assert_eq!(
            github_repo("git@github.com:acme/my.site.git"),
            r("acme", "my.site")
        );
        assert_eq!(
            github_repo("ssh://git@github.com/acme/shop.git"),
            r("acme", "shop")
        );
        assert_eq!(
            github_repo("ssh://git@github.com:22/acme/shop"),
            r("acme", "shop")
        );
        assert_eq!(
            github_repo("  HTTPS://GitHub.com/Acme/Shop.git\n"),
            r("Acme", "Shop")
        );
        for no in [
            "https://gitlab.com/acme/shop.git",
            "https://github.com.evil.example/acme/shop",
            "https://github.com/acme",
            "https://github.com/acme/shop/tree/main",
            "https://github.com/acme/..",
            "C:\\code\\shop",
            "/home/me/shop.git",
            "../origin.git",
            "file:///home/me/shop.git",
            "github.com/acme/shop",
        ] {
            assert_eq!(github_repo(no), None, "{no}");
        }
    }

    #[test]
    fn compare_links_open_a_pull_request_for_the_branch() {
        let repo = github_repo("git@github.com:acme/shop.git").expect("repo");
        assert_eq!(
            compare_url(&repo, Some("main"), "decurion/shop-7-fix"),
            "https://github.com/acme/shop/compare/main...decurion/shop-7-fix?expand=1"
        );
        let repo = github_repo("https://github.com/acme/shop").expect("repo");
        assert_eq!(
            compare_url(&repo, None, "decurion/shop-7"),
            "https://github.com/acme/shop/compare/decurion/shop-7?expand=1"
        );
        assert_eq!(
            compare_url(&repo, Some("rel #1"), "decurion/shop-7"),
            "https://github.com/acme/shop/compare/rel%20%231...decurion/shop-7?expand=1"
        );
    }

    #[test]
    fn passwords_are_taken_out_of_addresses_before_they_are_shown() {
        assert_eq!(
            scrub(
                "fatal: unable to access 'https://me:ghp_secret123@github.com/acme/shop.git/': 403"
            ),
            "fatal: unable to access 'https://github.com/acme/shop.git/': 403"
        );
        assert_eq!(scrub("https://ghp_x@github.com"), "https://github.com");
        assert_eq!(
            scrub("git@github.com:acme/shop.git"),
            "git@github.com:acme/shop.git"
        );
        assert_eq!(scrub("nothing here"), "nothing here");
    }

    #[test]
    fn a_token_is_never_in_the_push_or_its_log() {
        let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let args = push_args(&"a".repeat(40), "decurion/shop-7-fix");
        assert!(args.iter().all(|a| !a.contains(token) && !a.contains('@')));
        // The push names the remote, never its address, and is never forced.
        assert!(args.contains(&"origin".to_owned()));
        assert!(args
            .iter()
            .all(|a| !a.starts_with('+') && a != "--force" && a != "-f"));
        assert!(args
            .last()
            .is_some_and(|a| a.ends_with(":refs/heads/decurion/shop-7-fix")));
        assert_eq!(format!("{:?}", Token(token.into())), "Token(<hidden>)");
        let msg = push_failure(
            &scrub(&format!(
                "fatal: Authentication failed for 'https://me:{token}@github.com/a/b.git/'"
            )),
            "decurion/b-1",
            "GitHub",
        );
        assert!(!msg.contains(token), "{msg}");
        assert!(msg.contains("not signed in to GitHub"), "{msg}");
    }

    #[test]
    fn push_failures_are_told_in_plain_words() {
        let f = |e: &str| push_failure(e, "decurion/shop-7", "GitHub");
        assert!(
            f(" ! [rejected]  abc -> decurion/shop-7 (non-fast-forward)")
                .contains("already on GitHub with other changes")
        );
        assert!(f(" ! [remote rejected] abc -> decurion/shop-7 (protected)")
            .contains("refused the new branch"));
        assert!(f(
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled"
        )
        .contains("not signed in"));
        assert!(f("ERROR: Repository not found.").contains("not signed in"));
        assert!(f(
            "fatal: unable to access 'https://github.com/a/b/': Could not resolve host: github.com"
        )
        .contains("internet connection"));
        assert!(f("something odd").contains("Tap Try again"));
    }

    #[test]
    fn the_pull_request_says_what_was_done_and_who_made_it() {
        let s = Story {
            title: "Fix login".into(),
            goal: Some("Make sign-in work on phones".into()),
            summary: Some("Fixed the login form.".into()),
            review: Some(
                "Held for you: this changes protected parts of the app (src/auth.rs). Looks good."
                    .into(),
            ),
            held: true,
            reviewer: Some("param".into()),
            evidence: vec![Evidence {
                ran: "the app's tests".into(),
                result: "42 of 42 passed".into(),
            }],
            claims: vec![
                ReviewClaim {
                    text: "The login form works\non a phone.".into(),
                    how: Basis::Checked,
                },
                ReviewClaim {
                    text: "See https://me:secret@example.com/x for more.".into(),
                    how: Basis::Guessed,
                },
            ],
        };
        let body = pr_body(&s);
        assert!(body.contains("param reviews next."), "{body}");
        assert!(body.contains("## What was done\n\nFixed the login form."));
        assert!(body.contains("## The checker\n\nHeld for you:"));
        assert!(
            body.contains("Looks good.\n\nWhat it ran:\n\n- the app's tests: 42 of 42 passed"),
            "{body}"
        );
        assert!(body.contains(
            "What it found:\n\n- Checked: The login form works on a phone.\n- Guessed: See https://example.com/x for more."
        ));
        assert!(!body.contains("secret"), "{body}");
        assert!(body.contains("## Protected parts"));
        assert!(body.contains("Part of the goal: Make sign-in work on phones"));
        assert!(body.contains("Decurion made this pull request"));
        assert!(body.contains("Decurion never merges"));
        assert!(!body.contains('\u{2014}'), "no long dashes");
        let plain = pr_body(&Story {
            title: "x".into(),
            ..Story::default()
        });
        assert!(plain.contains("did not leave a summary"));
        assert!(plain.contains("No checker looked at this work"));
        assert!(!plain.contains("What it ran") && !plain.contains("What it found"));
        assert!(!plain.contains("Protected parts"));
        assert!(!plain.contains("reviews next"));
        assert_eq!(
            pr_title(&Story {
                title: "y".repeat(200),
                ..Story::default()
            })
            .chars()
            .count(),
            123
        );
    }
}
