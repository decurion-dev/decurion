//! One-tap deploy: after the owner merges, one Deploy tap puts the app live.
//!
//! An app's rules (Settings, the app's own page) can allow deploys and hold
//! its deploy steps and live address ([`decurion_proto::deploy`]). For such an
//! app the hub looks at the main line of its remote every [`POLL_SECS`]. When
//! that has moved past the last release, the board and the phone show one
//! "Ready to deploy" card for that exact commit, with the changes in it. One
//! tap runs the steps, in order, in a clean copy of that commit on the
//! owner's PC, and the card says how far it got and how it ended. Rules that
//! must stay true (ADR 0002, decided by the owner on 2026-10-18):
//!
//! - **Every deploy is the owner's tap.** Nothing deploys on a timer, on a
//!   merge, or for an agent, the coordinator, voice or a runner key. The
//!   routes are owner-only and not in `cloud_reach`. One tap, with a
//!   receipt, is the whole ask: there is no second "are you sure".
//! - **Only merged work, the exact commit shown.** The tap carries the commit
//!   the card showed; if the main line has moved on since, it is refused
//!   and the card moves to the newer commit ([`NEWER`]). The run checks again
//!   after fetching and releases nothing if the main line moved meanwhile.
//! - **Only the owner's own steps.** The steps come from the app's rules,
//!   which only the owner saves, and run as plain programs, never through a
//!   shell. Files the steps name (such as `scripts/deploy-prod.mjs`) are
//!   tracked: when one changed since the last release, the card names it and
//!   the button says "Deploy new steps", so the owner sees the change before
//!   the tap that runs it, and the run refuses if the file changed again
//!   after the card was drawn.
//! - **On the owner's PC, as the owner.** The steps run in the hub's own
//!   copy of the repo (`<data>/saved-work/deploys/app-N`), never in the
//!   owner's folder and never in an agent's box, with the PC's own sign-ins
//!   (such as the AWS command line's). The hub's own keys are taken out of
//!   their surroundings first ([`KEPT_OUT`]), and git never waits on a
//!   question.
//! - **Decurion never reads the app's data.** It starts the release and
//!   keeps what the steps print, which is the app's job to keep free of
//!   patient details. Addresses with passwords in them are scrubbed.
//! - **One deploy at a time, crash safe.** A deploy is marked running before
//!   anything runs. A hub that stops halfway marks it failed on its next
//!   start ([`recover`]) instead of running it again blindly, since half a
//!   release may be live; the owner checks and taps Try again.
//! - **Every tap and every end is written down** in `deploy_audit`.

use crate::checkpoints::git;
use crate::github::scrub;
use crate::push::{Notice, Pusher};
use crate::store::{check_receipt, receipt_result, save_receipt, Result, Store, StoreError};
use crate::{forbidden, ApiError, AppState, Caller};
use axum::extract::{Path as UrlPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use decurion_proto::deploy::{check_site, parse_steps, Step};
use serde::{Deserialize, Serialize};
use sqlx::{Row, Sqlite, Transaction};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// How often the main line of each deploying app is looked at.
pub const POLL_SECS: u64 = 120;

/// Longest a clone, fetch or look at the remote may take.
const GIT_SECS: u64 = 600;

/// Longest a whole deploy may run before it is stopped.
pub const RUN_SECS: u64 = 90 * 60;

/// Lines of output kept on the card for Details.
const TAIL_LINES: usize = 40;

/// Most output kept in a deploy's log file.
const MAX_LOG: u64 = 5 * 1024 * 1024;

/// Most change titles a card lists.
const MAX_CHANGES: usize = 20;

/// Cards about a finished release stay this long.
const LIVE_HOURS: i64 = 24;

/// Surroundings the steps never get: the hub's own keys and any AI or
/// GitHub key the hub was started with. Names starting with `DECURION_` are
/// taken out too.
const KEPT_OUT: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "OPENAI_API_KEY",
    "XAI_API_KEY",
    "GH_TOKEN",
    "GITHUB_TOKEN",
];

/// The main line moved on after the card was drawn.
pub const NEWER: &str =
    "A newer change was merged since this card was shown. Check the card again, then tap Deploy.";

const OFF: &str = "Deploys are off for this app, or its deploy steps are missing. Turn them on in the app's rules in Settings.";

const RUNNING_ELSEWHERE: &str =
    "Another app is deploying right now. Tap Deploy again when it has finished.";

const INTERRUPTED: &str = "Decurion stopped while this deploy was running, so part of it may be live. Check the site, then tap Try again.";

// ---------- Pure pieces ----------

/// A step as the owner wrote it, for the card and the log.
pub fn shown(step: &Step) -> String {
    step.iter()
        .map(|w| {
            if w.is_empty() || w.contains(' ') {
                format!("\"{w}\"")
            } else {
                w.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Files in the repo that the steps name, such as `scripts/deploy-prod.mjs`:
/// words after the program that look like a path inside the repo (a folder
/// in them, or a file ending), never an option, an address or a path that
/// leaves the repo. Sorted, each once.
pub fn named_files(steps: &[Step]) -> Vec<String> {
    let mut out: Vec<String> = steps
        .iter()
        .flat_map(|s| s.iter().skip(1))
        .filter_map(|w| {
            let w = w.replace('\\', "/");
            let w = w.trim_start_matches("./");
            let path_like = w.contains('/') || has_ending(w);
            let inside = !w.is_empty()
                && !w.starts_with('-')
                && !w.starts_with('/')
                && !w.contains(':')
                && !w.contains('=')
                && !w.split('/').any(|p| p == ".." || p.is_empty());
            (path_like && inside).then(|| w.to_owned())
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// "deploy.mjs", "run.json": a name, a dot, then 1-5 letters or digits.
fn has_ending(w: &str) -> bool {
    let name = w.rsplit('/').next().unwrap_or(w);
    match name.rsplit_once('.') {
        Some((stem, end)) => {
            !stem.is_empty()
                && (1..=5).contains(&end.len())
                && end.bytes().all(|b| b.is_ascii_alphanumeric())
                && end.bytes().any(|b| b.is_ascii_alphabetic())
        }
        None => false,
    }
}

/// Files whose hash differs between the last release and now, either way.
pub fn changed_files(
    before: &BTreeMap<String, String>,
    now: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut out: Vec<String> = now
        .iter()
        .filter(|(p, h)| before.get(*p) != Some(*h))
        .map(|(p, _)| p.clone())
        .chain(before.keys().filter(|p| !now.contains_key(*p)).cloned())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// A heading a deploy script prints for the owner, like
/// `=== 2/5 Snapshot ===`, turned into "2/5 Snapshot".
pub fn heading(line: &str) -> Option<String> {
    let t = line.trim();
    let inner = t.strip_prefix("===")?.strip_suffix("===")?.trim();
    (!inner.is_empty() && inner.chars().count() <= 120).then(|| inner.to_owned())
}

/// One line of output made fit to keep: colour codes and carriage-return
/// redraws taken out, passwords in addresses scrubbed.
pub fn clean_line(raw: &str) -> String {
    let last = raw
        .trim_end_matches(['\r', '\n'])
        .rsplit('\r')
        .next()
        .unwrap_or("");
    let mut out = String::with_capacity(last.len());
    let mut chars = last.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // `ESC [ ... letter`: a colour or cursor code.
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if c.is_control() && c != '\t' {
            continue;
        }
        out.push(c);
    }
    scrub(&out)
}

/// The program a step starts. On Windows, `npm` is `npm.cmd`: it is looked
/// for along PATH with the usual endings, the way a terminal would.
fn program(name: &str) -> PathBuf {
    if cfg!(windows) {
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                for end in ["exe", "cmd", "bat", "com"] {
                    let p = dir.join(format!("{name}.{end}"));
                    if p.is_file() {
                        return p;
                    }
                }
            }
        }
    }
    PathBuf::from(name)
}

/// Takes the hub's own keys out of a command's surroundings.
fn keep_keys_out(cmd: &mut Command) {
    for (k, _) in std::env::vars_os() {
        let name = k.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("DECURION_") || KEPT_OUT.contains(&name.as_str()) {
            cmd.env_remove(&k);
        }
    }
}

/// git that never waits on a question: no terminal, helper or SSH prompts,
/// no hooks, input closed.
fn quiet_git(dir: &Path) -> Command {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args([
            "-c",
            &format!("core.hooksPath={null}"),
            "-c",
            "core.fsmonitor=false",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    if std::env::var_os("GIT_SSH_COMMAND").is_none() && std::env::var_os("GIT_SSH").is_none() {
        cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    keep_keys_out(&mut cmd);
    cmd
}

/// Runs git in `dir` within [`GIT_SECS`]. The error is git's own message,
/// scrubbed, for the log; owner words are chosen by the caller.
async fn run_git(dir: &Path, args: &[&str]) -> std::result::Result<String, String> {
    let mut cmd = quiet_git(dir);
    cmd.args(args);
    match tokio::time::timeout(Duration::from_secs(GIT_SECS), cmd.output()).await {
        Err(_) => Err(format!("git {} took too long", args.first().unwrap_or(&""))),
        Ok(Err(e)) => Err(format!("could not start git: {e}")),
        Ok(Ok(out)) if out.status.success() => {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        }
        Ok(Ok(out)) => Err(scrub(String::from_utf8_lossy(&out.stderr).trim())),
    }
}

/// A branch name safe to put in a refspec.
fn plain_branch(b: &str) -> bool {
    (1..=100).contains(&b.len())
        && b.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'/' | b'.' | b'_' | b'-'))
        && !b.starts_with(['-', '/', '.'])
        && !b.contains("..")
        && !b.ends_with(['/', '.'])
}

fn is_sha(s: &str) -> bool {
    (40..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `git ls-remote --symref origin HEAD`, read: the remote's main line and
/// the commit at its tip.
pub fn read_symref(out: &str) -> Option<(String, String)> {
    let mut branch = None;
    let mut commit = None;
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("ref: refs/heads/") {
            branch = rest.split('\t').next().map(str::to_owned);
        } else if let Some((sha, name)) = line.split_once('\t') {
            if name == "HEAD" && is_sha(sha) {
                commit = Some(sha.to_owned());
            }
        }
    }
    Some((branch.filter(|b| plain_branch(b))?, commit?))
}

// ---------- Store ----------

/// An app's deploy rule, when deploys are on and the steps read cleanly.
#[derive(Debug, Clone)]
pub(crate) struct Rule {
    pub repo_path: String,
    pub steps: Vec<Step>,
}

fn rule_from(row: &sqlx::sqlite::SqliteRow) -> Result<Option<Rule>> {
    if !row.try_get::<bool, _>("can_deploy")? {
        return Ok(None);
    }
    let Ok(steps) = parse_steps(&row.try_get::<String, _>("deploy_steps")?) else {
        return Ok(None);
    };
    Ok(Some(Rule {
        repo_path: row.try_get("repo_path")?,
        steps,
    }))
}

/// What a look at the remote found.
#[derive(Debug, Clone)]
pub(crate) struct Head {
    pub branch: String,
    pub commit: String,
    pub changes: Vec<String>,
    pub files: BTreeMap<String, String>,
}

/// A deploy card, as the board, the phone and the Today strip show it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Card {
    pub id: i64,
    pub app_id: i64,
    pub app: String,
    /// `waiting`, `deploying`, `live` or `failed`.
    pub state: String,
    pub commit: String,
    /// The first 7 characters of the commit.
    pub short: String,
    pub branch: String,
    /// Titles of the changes since the last release, newest first.
    pub changes: Vec<String>,
    /// The app has been released from Decurion before.
    pub released_before: bool,
    /// The steps, one per line, as the owner wrote them.
    pub steps: Vec<String>,
    /// Files the steps run that changed since the last release.
    pub steps_changed: Vec<String>,
    /// Where the app goes live, for Open site.
    pub site: Option<String>,
    /// How far a running deploy has got.
    pub step: Option<String>,
    pub error: Option<String>,
    /// The last lines of output, for Details.
    pub tail: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    /// One or two words: "Ready to deploy", "Deploying", "Live", "Deploy failed".
    pub status: String,
    /// What the card says, in a short sentence.
    pub line: String,
    /// The button's words, when there is one: "Deploy", "Deploy new steps",
    /// "Try again".
    pub button: Option<String>,
}

/// The words on a card, chosen once here so every screen says the same.
pub fn card_words(c: &Card) -> (String, String, Option<String>) {
    match c.state.as_str() {
        "waiting" => {
            let line = if !c.steps_changed.is_empty() {
                format!("Deploy steps changed: {}", c.steps_changed.join(", "))
            } else if !c.released_before {
                "Ready for its first deploy from Decurion".to_owned()
            } else {
                match c.changes.len() {
                    0 => "Ready to deploy".to_owned(),
                    1 => format!("1 change: {}", c.changes[0]),
                    n if n >= MAX_CHANGES => format!("{n} or more changes since the last release"),
                    n => format!("{n} changes since the last release"),
                }
            };
            let button = if c.steps_changed.is_empty() {
                "Deploy"
            } else {
                "Deploy new steps"
            };
            ("Ready to deploy".into(), line, Some(button.into()))
        }
        "deploying" => (
            "Deploying".into(),
            c.step.clone().unwrap_or_else(|| "Getting the code".into()),
            None,
        ),
        "failed" => (
            "Deploy failed".into(),
            c.error
                .clone()
                .unwrap_or_else(|| "The deploy did not finish.".into()),
            Some("Try again".into()),
        ),
        _ => ("Live".into(), format!("{} is live", c.short), None),
    }
}

fn json_list(s: &str) -> Vec<String> {
    serde_json::from_str(s).unwrap_or_default()
}

fn json_map(s: &str) -> BTreeMap<String, String> {
    serde_json::from_str(s).unwrap_or_default()
}

async fn note(
    tx: &mut Transaction<'_, Sqlite>,
    deploy_id: i64,
    actor: &str,
    what: &str,
    note: Option<&str>,
) -> Result<()> {
    sqlx::query("INSERT INTO deploy_audit (deploy_id, actor, what, note) VALUES (?, ?, ?, ?)")
        .bind(deploy_id)
        .bind(actor)
        .bind(what)
        .bind(note)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

const CARD_COLS: &str = "d.id, d.app_id, a.name, d.state, d.commit_id, d.branch, d.changes,
     d.steps_changed, d.step, d.error, d.tail, d.created_at, d.started_at, d.ended_at,
     a.deploy_steps, a.deploy_site,
     EXISTS (SELECT 1 FROM deploys l WHERE l.app_id = d.app_id AND l.state = 'live'
             AND l.id <> d.id) AS released_before";

fn card_from(r: &sqlx::sqlite::SqliteRow) -> Result<Card> {
    let commit: String = r.try_get("commit_id")?;
    let site: Option<String> = r.try_get("deploy_site")?;
    let mut c = Card {
        id: r.try_get("id")?,
        app_id: r.try_get("app_id")?,
        app: r.try_get("name")?,
        state: r.try_get("state")?,
        short: commit.chars().take(7).collect(),
        commit,
        branch: r.try_get("branch")?,
        changes: json_list(r.try_get("changes")?),
        released_before: r.try_get("released_before")?,
        steps: r
            .try_get::<String, _>("deploy_steps")?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect(),
        steps_changed: json_list(r.try_get("steps_changed")?),
        site: site.and_then(|s| check_site(&s).ok()),
        step: r.try_get("step")?,
        error: r.try_get("error")?,
        tail: r.try_get("tail")?,
        created_at: r.try_get("created_at")?,
        started_at: r.try_get("started_at")?,
        ended_at: r.try_get("ended_at")?,
        status: String::new(),
        line: String::new(),
        button: None,
    };
    (c.status, c.line, c.button) = card_words(&c);
    Ok(c)
}

/// What a run needs, read once when it starts.
struct Job {
    app_id: i64,
    branch: String,
    commit: String,
    files: BTreeMap<String, String>,
    rule: Option<Rule>,
}

impl Store {
    /// App `app_id`'s deploy rule, if deploys are on and its steps read.
    pub(crate) async fn deploy_rule(&self, app_id: i64) -> Result<Option<Rule>> {
        let row = sqlx::query("SELECT repo_path, can_deploy, deploy_steps FROM apps WHERE id = ? AND removed_at IS NULL")
            .bind(app_id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(StoreError::NotFound)?;
        rule_from(&row)
    }

    /// Apps whose rules allow deploys.
    async fn deploying_apps(&self) -> Result<Vec<i64>> {
        Ok(sqlx::query_scalar(
            "SELECT id FROM apps WHERE can_deploy = 1 AND removed_at IS NULL ORDER BY id",
        )
        .fetch_all(self.pool())
        .await?)
    }

    /// The commit of app `app_id`'s last release from Decurion.
    async fn last_live(&self, app_id: i64) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "SELECT commit_id FROM deploys WHERE app_id = ? AND state = 'live'
             ORDER BY id DESC LIMIT 1",
        )
        .bind(app_id)
        .fetch_optional(self.pool())
        .await?)
    }

    /// A deploy of app `app_id` is running now.
    async fn deploy_running(&self, app_id: i64) -> Result<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM deploys WHERE app_id = ? AND state = 'deploying')",
        )
        .bind(app_id)
        .fetch_one(self.pool())
        .await?)
    }

    /// Saves what a look at the remote found. The app's one open card moves
    /// to the new commit (a newer merge never adds a second ask), or a new
    /// card is made when there is none and the commit is neither the last
    /// release nor one the owner said Not now to. Returns the card when it is
    /// new or moved, so the owner can hear about it.
    pub(crate) async fn note_head(&self, app_id: i64, h: &Head) -> Result<Option<i64>> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let open: Option<(i64, String, String)> = sqlx::query_as(
            "SELECT id, state, commit_id FROM deploys
             WHERE app_id = ? AND state IN ('waiting', 'deploying', 'failed')",
        )
        .bind(app_id)
        .fetch_optional(&mut *tx)
        .await?;
        let live: Option<(String, String)> = sqlx::query_as(
            "SELECT commit_id, files FROM deploys WHERE app_id = ? AND state = 'live'
             ORDER BY id DESC LIMIT 1",
        )
        .bind(app_id)
        .fetch_optional(&mut *tx)
        .await?;
        let steps_changed = live
            .as_ref()
            .map(|(_, f)| changed_files(&json_map(f), &h.files))
            .unwrap_or_default();
        let changes = serde_json::to_string(&h.changes).unwrap_or_else(|_| "[]".into());
        let files = serde_json::to_string(&h.files).unwrap_or_else(|_| "{}".into());
        let steps_changed = serde_json::to_string(&steps_changed).unwrap_or_else(|_| "[]".into());
        let out = match open {
            Some((_, state, _)) if state == "deploying" => None,
            Some((id, _, commit)) if commit == h.commit => {
                sqlx::query(
                    "UPDATE deploys SET changes = ?, files = ?, steps_changed = ? WHERE id = ?",
                )
                .bind(&changes)
                .bind(&files)
                .bind(&steps_changed)
                .bind(id)
                .execute(&mut *tx)
                .await?;
                None
            }
            Some((id, _, _)) => {
                sqlx::query(
                    "UPDATE deploys SET branch = ?, commit_id = ?, changes = ?, files = ?,
                         steps_changed = ?, state = 'waiting', step = NULL, error = NULL,
                         tail = NULL, started_at = NULL, ended_at = NULL,
                         created_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                     WHERE id = ?",
                )
                .bind(&h.branch)
                .bind(&h.commit)
                .bind(&changes)
                .bind(&files)
                .bind(&steps_changed)
                .bind(id)
                .execute(&mut *tx)
                .await?;
                note(
                    &mut tx,
                    id,
                    "hub",
                    "moved to a newer merge",
                    Some(&h.commit),
                )
                .await?;
                Some(id)
            }
            None => {
                let hidden: Option<String> = sqlx::query_scalar(
                    "SELECT commit_id FROM deploys WHERE app_id = ? AND state = 'hidden'
                     ORDER BY id DESC LIMIT 1",
                )
                .bind(app_id)
                .fetch_optional(&mut *tx)
                .await?;
                let released = live.as_ref().is_some_and(|(c, _)| *c == h.commit);
                if released || hidden.as_deref() == Some(h.commit.as_str()) {
                    None
                } else {
                    let id: i64 = sqlx::query_scalar(
                        "INSERT INTO deploys (app_id, branch, commit_id, changes, files, steps_changed)
                         VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
                    )
                    .bind(app_id)
                    .bind(&h.branch)
                    .bind(&h.commit)
                    .bind(&changes)
                    .bind(&files)
                    .bind(&steps_changed)
                    .fetch_one(&mut *tx)
                    .await?;
                    note(&mut tx, id, "hub", "ready to deploy", Some(&h.commit)).await?;
                    Some(id)
                }
            }
        };
        tx.commit().await?;
        Ok(out)
    }

    /// One card by its id.
    pub async fn deploy_card(&self, id: i64) -> Result<Card> {
        let r = sqlx::query(&format!(
            "SELECT {CARD_COLS} FROM deploys d JOIN apps a ON a.id = d.app_id WHERE d.id = ?"
        ))
        .bind(id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(StoreError::NotFound)?;
        card_from(&r)
    }

    /// The cards to show: each app's open card (a waiting or failed one only
    /// while its rules still allow deploys), else its newest release from
    /// the last day.
    pub async fn deploy_cards(&self) -> Result<Vec<Card>> {
        let rows = sqlx::query(&format!(
            "SELECT {CARD_COLS} FROM deploys d JOIN apps a ON a.id = d.app_id
             WHERE (d.state = 'deploying')
                OR (d.state IN ('waiting', 'failed') AND a.can_deploy = 1)
                OR (d.state = 'live'
                    AND d.ended_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-{LIVE_HOURS} hours')
                    AND d.id = (SELECT MAX(x.id) FROM deploys x
                                WHERE x.app_id = d.app_id AND x.state <> 'hidden')
                    AND NOT EXISTS (SELECT 1 FROM deploys o WHERE o.app_id = d.app_id
                                    AND o.state IN ('waiting', 'deploying', 'failed')))
             ORDER BY a.name, d.id"
        ))
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(card_from).collect()
    }

    /// The owner's Deploy (or Try again) on card `id`, for exactly `commit`.
    /// With a receipt already used for this card, nothing starts again and
    /// the card is returned as it is; the flag is false then.
    pub async fn start_deploy(
        &self,
        id: i64,
        commit: &str,
        receipt: Option<&str>,
    ) -> Result<(Card, bool)> {
        check_receipt(receipt)?;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(done) = receipt_result(&mut tx, receipt, "deploy").await? {
            if done != id {
                return Err(StoreError::Duplicate(crate::store::RECEIPT_REUSED.into()));
            }
            tx.commit().await?;
            return Ok((self.deploy_card(id).await?, false));
        }
        let row = sqlx::query(
            "SELECT d.state, d.commit_id, a.name, a.repo_path, a.can_deploy, a.deploy_steps,
                    a.deploy_site
             FROM deploys d JOIN apps a ON a.id = d.app_id WHERE d.id = ?",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::NotFound)?;
        let state: String = row.try_get("state")?;
        match state.as_str() {
            "waiting" | "failed" => {}
            "deploying" => {
                return Err(StoreError::Refused(
                    "This deploy is already running.".into(),
                ))
            }
            _ => {
                return Err(StoreError::Refused(
                    "This deploy is over. Check the app's card for the newest change.".into(),
                ))
            }
        }
        if row.try_get::<String, _>("commit_id")? != commit {
            return Err(StoreError::Refused(NEWER.into()));
        }
        if rule_from(&row)?.is_none() {
            return Err(StoreError::Refused(OFF.into()));
        }
        let busy: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM deploys WHERE state = 'deploying')")
                .fetch_one(&mut *tx)
                .await?;
        if busy {
            return Err(StoreError::Refused(RUNNING_ELSEWHERE.into()));
        }
        sqlx::query(
            "UPDATE deploys SET state = 'deploying', step = 'Getting the code', error = NULL,
                 tail = NULL, ended_at = NULL,
                 started_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        let what = if state == "failed" {
            "Try again tapped"
        } else {
            "Deploy tapped"
        };
        note(&mut tx, id, "owner", what, Some(commit)).await?;
        save_receipt(&mut tx, receipt, "deploy", id).await?;
        tx.commit().await?;
        Ok((self.deploy_card(id).await?, true))
    }

    /// The owner's Not now: the card goes away until the next merge.
    pub async fn hide_deploy(&self, id: i64, receipt: Option<&str>) -> Result<Card> {
        check_receipt(receipt)?;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        if let Some(done) = receipt_result(&mut tx, receipt, "deploy_hide").await? {
            if done != id {
                return Err(StoreError::Duplicate(crate::store::RECEIPT_REUSED.into()));
            }
            tx.commit().await?;
            return self.deploy_card(id).await;
        }
        let done = sqlx::query(
            "UPDATE deploys SET state = 'hidden' WHERE id = ? AND state IN ('waiting', 'failed')",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if done.rows_affected() == 0 {
            let there: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM deploys WHERE id = ?)")
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?;
            return Err(if there {
                StoreError::Refused("This deploy is running or over, so it stays.".into())
            } else {
                StoreError::NotFound
            });
        }
        note(&mut tx, id, "owner", "Not now tapped", None).await?;
        save_receipt(&mut tx, receipt, "deploy_hide", id).await?;
        tx.commit().await?;
        self.deploy_card(id).await
    }

    async fn deploy_job(&self, id: i64) -> Result<Job> {
        let row = sqlx::query(
            "SELECT d.app_id, d.branch, d.commit_id, d.files, a.name, a.repo_path, a.can_deploy,
                    a.deploy_steps, a.deploy_site
             FROM deploys d JOIN apps a ON a.id = d.app_id WHERE d.id = ?",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(StoreError::NotFound)?;
        Ok(Job {
            app_id: row.try_get("app_id")?,
            branch: row.try_get("branch")?,
            commit: row.try_get("commit_id")?,
            files: json_map(row.try_get("files")?),
            rule: rule_from(&row)?,
        })
    }

    async fn deploy_step(&self, id: i64, words: &str) -> Result<()> {
        sqlx::query("UPDATE deploys SET step = ? WHERE id = ? AND state = 'deploying'")
            .bind(words)
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    async fn end_deploy(
        &self,
        id: i64,
        live: bool,
        error: Option<&str>,
        tail: Option<&str>,
    ) -> Result<()> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "UPDATE deploys SET state = ?, error = ?, tail = ?, step = NULL,
                 ended_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ? AND state = 'deploying'",
        )
        .bind(if live { "live" } else { "failed" })
        .bind(error)
        .bind(tail)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        let what = if live { "live" } else { "failed" };
        note(&mut tx, id, "hub", what, error).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Deploys left running by a hub that stopped are marked failed, never
    /// run again by themselves. Returns how many.
    pub async fn recover_deploys(&self) -> Result<usize> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM deploys WHERE state = 'deploying'")
            .fetch_all(&mut *tx)
            .await?;
        for id in &ids {
            sqlx::query(
                "UPDATE deploys SET state = 'failed', error = ?, step = NULL,
                     ended_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                 WHERE id = ?",
            )
            .bind(INTERRUPTED)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            note(&mut tx, *id, "hub", "interrupted", None).await?;
        }
        tx.commit().await?;
        Ok(ids.len())
    }

    /// Whether each goal of apps that deploy is live, by goal: true once
    /// every merged pull request of the goal is in a release from Decurion,
    /// false while one is merged but not released. Goals of apps that do not
    /// deploy, and goals with nothing merged, are left out. `app_id` limits
    /// it to one app.
    pub async fn goals_live(&self, app_id: Option<i64>) -> Result<HashMap<i64, bool>> {
        let rows: Vec<(i64, i64, i64)> = sqlx::query_as(
            "SELECT p.goal_id, COUNT(*), SUM(w.live_at IS NOT NULL)
             FROM goal_pieces p JOIN pr_watches w ON w.thread_id = p.thread_id
             JOIN goals g ON g.id = p.goal_id JOIN apps a ON a.id = g.app_id
             WHERE a.can_deploy = 1 AND p.merged_at IS NOT NULL
               AND (?1 IS NULL OR g.app_id = ?1)
             GROUP BY p.goal_id",
        )
        .bind(app_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(goal, merged, live)| (goal, live >= merged))
            .collect())
    }

    /// [`Store::goals_live`] for one goal.
    pub async fn goal_live(&self, goal_id: i64) -> Result<Option<bool>> {
        let app: Option<i64> = sqlx::query_scalar("SELECT app_id FROM goals WHERE id = ?")
            .bind(goal_id)
            .fetch_optional(self.pool())
            .await?;
        let Some(app) = app else { return Ok(None) };
        Ok(self.goals_live(Some(app)).await?.get(&goal_id).copied())
    }

    /// Merged pull requests of app `app_id` not yet known to be live.
    async fn not_yet_live(
        &self,
        app_id: i64,
    ) -> Result<Vec<(i64, Option<String>, Option<String>)>> {
        Ok(sqlx::query_as(
            "SELECT thread_id, merge_commit, event_at FROM pr_watches
             WHERE app_id = ? AND state = 'merged' AND live_at IS NULL",
        )
        .bind(app_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// When deploy `id` started.
    async fn deploy_started(&self, id: i64) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT started_at FROM deploys WHERE id = ?")
                .bind(id)
                .fetch_optional(self.pool())
                .await?
                .flatten(),
        )
    }

    /// When app `app_id`'s last release from Decurion started.
    async fn last_live_start(&self, app_id: i64) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "SELECT started_at FROM deploys WHERE app_id = ? AND state = 'live'
             ORDER BY id DESC LIMIT 1",
        )
        .bind(app_id)
        .fetch_optional(self.pool())
        .await?
        .flatten())
    }

    async fn set_pr_live(&self, thread_id: i64) -> Result<()> {
        sqlx::query(
            "UPDATE pr_watches SET live_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE thread_id = ? AND live_at IS NULL",
        )
        .bind(thread_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// What happened to deploy `id`, oldest first: (actor, what).
    pub async fn deploy_history(&self, id: i64) -> Result<Vec<(String, String)>> {
        Ok(
            sqlx::query_as("SELECT actor, what FROM deploy_audit WHERE deploy_id = ? ORDER BY id")
                .bind(id)
                .fetch_all(self.pool())
                .await?,
        )
    }
}

// ---------- Looking and running ----------

/// Lets one look or deploy use the hub's deploy copies at a time.
pub type Busy = Arc<tokio::sync::Mutex<()>>;

/// What looking and deploying need.
#[derive(Clone)]
pub struct Deployer {
    pub(crate) store: Store,
    /// `<data>/saved-work/deploys`.
    pub(crate) dir: PathBuf,
    pub(crate) push: Option<Pusher>,
    pub(crate) busy: Busy,
}

impl Deployer {
    fn copy_dir(&self, app_id: i64) -> PathBuf {
        self.dir.join(format!("app-{app_id}"))
    }

    fn log_path(&self, id: i64) -> PathBuf {
        self.dir.join("logs").join(format!("{id}.log"))
    }

    async fn tell(&self, n: Notice) {
        if let Some(p) = &self.push {
            p.send(&self.store, &n).await;
        }
    }

    /// Makes the hub's copy of the app's repo the first time: a local clone
    /// of the owner's folder (so nothing big comes over the network), then
    /// pointed at the folder's own remote. Nothing is written into the
    /// owner's folder.
    async fn ensure_copy(
        &self,
        app_id: i64,
        repo_path: &str,
    ) -> std::result::Result<PathBuf, String> {
        let dir = self.copy_dir(app_id);
        let folder = Path::new(repo_path);
        let remote = git(folder, &["remote", "get-url", "origin"])
            .await
            .map_err(|_| {
                "This app's folder has no remote to deploy from. Connect it to GitHub first."
                    .to_owned()
            })?;
        if !dir.join(".git").exists() {
            if dir.exists() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
            let mut cmd = quiet_git(&self.dir);
            cmd.args(["clone", "--quiet", "--no-checkout", "--"])
                .arg(folder)
                .arg(&dir);
            let out = tokio::time::timeout(Duration::from_secs(GIT_SECS), cmd.output())
                .await
                .map_err(|_| "Copying the app's code took too long.".to_owned())?
                .map_err(|e| format!("could not start git: {e}"))?;
            if !out.status.success() {
                tracing::warn!(app = app_id, error = %scrub(String::from_utf8_lossy(&out.stderr).trim()), "could not copy the app for deploys");
                return Err("Decurion could not copy the app's code from its folder.".into());
            }
        }
        run_git(&dir, &["remote", "set-url", "origin", &remote])
            .await
            .map_err(|e| {
                tracing::warn!(app = app_id, error = %e, "could not point the deploy copy at the remote");
                "Decurion could not set up its copy of the app's code.".to_owned()
            })?;
        Ok(dir)
    }

    /// Fetches the remote's main line into the copy. Returns (branch, commit).
    async fn fetch_main(&self, dir: &Path) -> std::result::Result<(String, String), String> {
        let out = run_git(dir, &["ls-remote", "--symref", "origin", "HEAD"])
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "could not look at the app's remote");
                "Decurion could not reach the app's remote. Check this PC's internet and GitHub sign-in.".to_owned()
            })?;
        let (branch, commit) = read_symref(&out)
            .ok_or_else(|| "The app's remote did not say what its main line is.".to_owned())?;
        let spec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
        run_git(dir, &["fetch", "--quiet", "--no-tags", "origin", &spec])
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "could not fetch the app's main line");
                "Decurion could not fetch the app's newest code.".to_owned()
            })?;
        let tip = run_git(
            dir,
            &["rev-parse", &format!("refs/remotes/origin/{branch}")],
        )
        .await
        .map_err(|_| "Decurion could not read the app's newest code.".to_owned())?;
        // A push between the look and the fetch: the fetched tip wins.
        let commit = if is_sha(&tip) { tip } else { commit };
        Ok((branch, commit))
    }

    /// Hashes of the files the steps name, at `commit`. A missing file has
    /// no entry.
    async fn file_hashes(dir: &Path, commit: &str, steps: &[Step]) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for f in named_files(steps) {
            if let Ok(h) = run_git(
                dir,
                &["rev-parse", "--verify", "--quiet", &format!("{commit}:{f}")],
            )
            .await
            {
                if is_sha(&h) {
                    out.insert(f, h);
                }
            }
        }
        out
    }

    /// Marks merged pull requests of app `app_id` live once its last release
    /// holds their merge commit. A pull request merged before Decurion kept
    /// merge commits counts as live when that release started after the merge
    /// was seen (a release always fetches the newest main line first).
    async fn mark_live(&self, app_id: i64, dir: &Path) {
        let (Ok(Some(live)), Ok(start)) = (
            self.store.last_live(app_id).await,
            self.store.last_live_start(app_id).await,
        ) else {
            return;
        };
        self.mark_held(app_id, dir, &live, start).await;
    }

    /// [`Deployer::mark_live`] for a release of commit `live` that started at
    /// `start`, before it is recorded, so the card never says Live while its
    /// goals still say ready to deploy.
    async fn mark_held(&self, app_id: i64, dir: &Path, live: &str, start: Option<String>) {
        let Ok(waiting) = self.store.not_yet_live(app_id).await else {
            return;
        };
        for (thread_id, merge, seen) in waiting {
            let held = match merge.filter(|m| is_sha(m)) {
                Some(m) => run_git(dir, &["merge-base", "--is-ancestor", &m, live])
                    .await
                    .is_ok(),
                None => matches!((&seen, &start), (Some(seen), Some(start)) if seen <= start),
            };
            if held {
                if let Err(e) = self.store.set_pr_live(thread_id).await {
                    tracing::warn!(error = %e, thread = thread_id, "could not mark a pull request live");
                    continue;
                }
                // The last step of its goal, maybe: the goal closes by itself.
                match self.store.piece_live(thread_id).await {
                    Ok(Some(n)) => {
                        if let Some(p) = &self.push {
                            p.send(&self.store, &n).await;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, thread = thread_id, "could not close a goal that is live")
                    }
                }
            }
        }
    }

    /// Looks at app `app_id`'s main line once and updates its card. Skipped
    /// while that app is deploying.
    pub async fn look(&self, app_id: i64) -> std::result::Result<Option<i64>, String> {
        let rule = match self.store.deploy_rule(app_id).await {
            Ok(Some(r)) => r,
            Ok(None) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let Ok(_held) = self.busy.try_lock() else {
            return Ok(None);
        };
        if self.store.deploy_running(app_id).await.unwrap_or(true) {
            return Ok(None);
        }
        let dir = self.ensure_copy(app_id, &rule.repo_path).await?;
        let (branch, commit) = self.fetch_main(&dir).await?;
        let live = self
            .store
            .last_live(app_id)
            .await
            .map_err(|e| e.to_string())?;
        let range = match &live {
            Some(l)
                if run_git(&dir, &["merge-base", "--is-ancestor", l, &commit])
                    .await
                    .is_ok() =>
            {
                format!("{l}..{commit}")
            }
            _ => commit.clone(),
        };
        let shown_max = if live.is_some() { MAX_CHANGES } else { 5 };
        let changes = run_git(
            &dir,
            &[
                "log",
                "--format=%s",
                &format!("-n{shown_max}"),
                &range,
                "--",
            ],
        )
        .await
        .unwrap_or_default()
        .lines()
        .map(|l| l.chars().take(120).collect::<String>())
        .filter(|l| !l.trim().is_empty())
        .collect();
        let files = Self::file_hashes(&dir, &commit, &rule.steps).await;
        let head = Head {
            branch,
            commit,
            changes,
            files,
        };
        let moved = self
            .store
            .note_head(app_id, &head)
            .await
            .map_err(|e| e.to_string())?;
        self.mark_live(app_id, &dir).await;
        if let Some(id) = moved {
            if let Ok(c) = self.store.deploy_card(id).await {
                self.tell(notice(&c, "ready to deploy", &c.line)).await;
            }
        }
        Ok(moved)
    }

    /// Looks at every app whose rules allow deploys.
    pub async fn look_all(&self) {
        let apps = match self.store.deploying_apps().await {
            Ok(a) => a,
            Err(e) => {
                tracing::error!(error = %e, "could not list the apps that deploy");
                return;
            }
        };
        for app in apps {
            if let Err(why) = self.look(app).await {
                tracing::warn!(app, reason = %why, "could not look for changes to deploy");
            }
        }
    }

    /// Runs deploy `id`, already marked running by the owner's tap. Never
    /// fails the caller: the end is saved on the card.
    pub async fn run(&self, id: i64) {
        let _one = self.busy.lock().await;
        let (ok, error, tail) = match self.attempt(id).await {
            Ok(tail) => (true, None, tail),
            Err((why, tail)) => (false, Some(why), tail),
        };
        let tail = (!tail.is_empty()).then(|| tail.join("\n"));
        if ok {
            if let (Ok(c), Ok(start)) = (
                self.store.deploy_card(id).await,
                self.store.deploy_started(id).await,
            ) {
                self.mark_held(c.app_id, &self.copy_dir(c.app_id), &c.commit, start)
                    .await;
            }
        }
        if let Err(e) = self
            .store
            .end_deploy(id, ok, error.as_deref(), tail.as_deref())
            .await
        {
            tracing::error!(error = %e, deploy = id, "could not record the end of a deploy");
            return;
        }
        tracing::info!(deploy = id, live = ok, "deploy finished");
        if let Ok(c) = self.store.deploy_card(id).await {
            let n = if ok {
                notice(&c, "live", &format!("{} is live", c.short))
            } else {
                notice(&c, "deploy failed", &c.line)
            };
            self.tell(n).await;
        }
    }

    async fn attempt(&self, id: i64) -> std::result::Result<Vec<String>, (String, Vec<String>)> {
        let none = Vec::new;
        let job = self.store.deploy_job(id).await.map_err(|e| {
            tracing::error!(error = %e, deploy = id, "could not read the deploy");
            (
                "Decurion could not read this deploy. Tap Try again.".to_owned(),
                none(),
            )
        })?;
        let rule = job.rule.ok_or_else(|| (OFF.to_owned(), none()))?;
        let dir = self
            .ensure_copy(job.app_id, &rule.repo_path)
            .await
            .map_err(|e| (e, none()))?;
        let (branch, commit) = self.fetch_main(&dir).await.map_err(|e| (e, none()))?;
        if branch != job.branch || commit != job.commit {
            return Err((
                "A newer change was merged while this deploy was starting, so nothing was released. Tap Deploy on the new card.".into(),
                none(),
            ));
        }
        let start = format!("refs/remotes/origin/{branch}");
        for args in [
            vec!["checkout", "--quiet", "--force", "-B", &branch, &commit],
            vec!["branch", "--quiet", "--set-upstream-to", &start],
            vec!["reset", "--quiet", "--hard", &commit],
            vec!["clean", "-ffdxq", "-e", "node_modules"],
        ] {
            run_git(&dir, &args).await.map_err(|e| {
                tracing::warn!(deploy = id, error = %e, "could not prepare the deploy copy");
                (
                    "Decurion could not get a clean copy of the code. Tap Try again.".to_owned(),
                    none(),
                )
            })?;
        }
        if Self::file_hashes(&dir, &commit, &rule.steps).await != job.files {
            return Err((
                "The deploy steps changed after this card was shown, so nothing ran. Check the card again.".into(),
                none(),
            ));
        }
        let log_path = self.log_path(id);
        let mut log = Log::open(&log_path).await;
        let deadline = Instant::now() + Duration::from_secs(RUN_SECS);
        let total = rule.steps.len();
        let mut tail: VecDeque<String> = VecDeque::new();
        for (i, step) in rule.steps.iter().enumerate() {
            let words = shown(step);
            let at = format!("Step {} of {total}: {words}", i + 1);
            let _ = self.store.deploy_step(id, &at).await;
            log.line(&format!("$ {words}")).await;
            let r = self
                .run_step(id, &dir, step, &at, deadline, &mut log, &mut tail)
                .await;
            if let Err(why) = r {
                log.line(&format!("Stopped: {why}")).await;
                return Err((why, tail.into()));
            }
        }
        log.line("Deploy finished.").await;
        Ok(tail.into())
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_step(
        &self,
        id: i64,
        dir: &Path,
        step: &Step,
        at: &str,
        deadline: Instant,
        log: &mut Log,
        tail: &mut VecDeque<String>,
    ) -> std::result::Result<(), String> {
        let name = &step[0];
        let mut cmd = Command::new(program(name));
        cmd.args(&step[1..])
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "never")
            .env("NO_COLOR", "1")
            .env("FORCE_COLOR", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        keep_keys_out(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| {
            tracing::warn!(deploy = id, program = %name, error = %e, "could not start a deploy step");
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("Decurion could not find {name} on this PC. Install it, then tap Try again.")
            } else {
                format!("Decurion could not start {name}. Tap Try again.")
            }
        })?;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(256);
        if let Some(out) = child.stdout.take() {
            tokio::spawn(lines(out, tx.clone()));
        }
        if let Some(err) = child.stderr.take() {
            tokio::spawn(lines(err, tx.clone()));
        }
        drop(tx);
        let left = deadline.saturating_duration_since(Instant::now());
        let reading = async {
            let mut last_heading = String::new();
            while let Some(line) = rx.recv().await {
                if let Some(h) = heading(&line) {
                    if h != last_heading {
                        let _ = self.store.deploy_step(id, &format!("{at} · {h}")).await;
                        last_heading = h;
                    }
                }
                log.line(&line).await;
                tail.push_back(line);
                while tail.len() > TAIL_LINES {
                    tail.pop_front();
                }
            }
            child.wait().await
        };
        let status = match tokio::time::timeout(left, reading).await {
            Err(_) => {
                return Err(format!(
                    "The deploy took longer than {} minutes, so Decurion stopped it. Check the site, then tap Try again.",
                    RUN_SECS / 60
                ))
            }
            Ok(Err(e)) => {
                tracing::warn!(deploy = id, error = %e, "lost a deploy step");
                return Err("Decurion lost track of a deploy step. Check the site, then tap Try again.".into());
            }
            Ok(Ok(s)) => s,
        };
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "{} stopped with an error, so the release did not finish. Open Details to see why, then tap Try again.",
                at.split(": ").next().unwrap_or("A step")
            ))
        }
    }
}

/// Reads a stream line by line into `tx`, each line cleaned.
async fn lines<R: AsyncRead + Unpin>(r: R, tx: tokio::sync::mpsc::Sender<String>) {
    let mut r = BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match r.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = clean_line(&String::from_utf8_lossy(&buf));
                if !line.trim().is_empty() && tx.send(line).await.is_err() {
                    break;
                }
            }
        }
    }
}

/// A deploy's log file, cut off at [`MAX_LOG`]. Writing never fails a
/// deploy: a log that cannot be written is only missed.
struct Log {
    file: Option<tokio::fs::File>,
    written: u64,
}

impl Log {
    async fn open(path: &Path) -> Self {
        if let Some(p) = path.parent() {
            let _ = tokio::fs::create_dir_all(p).await;
        }
        Self {
            file: tokio::fs::File::create(path).await.ok(),
            written: 0,
        }
    }

    async fn line(&mut self, text: &str) {
        let Some(f) = self.file.as_mut() else { return };
        if self.written >= MAX_LOG {
            return;
        }
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(b'\n');
        self.written += bytes.len() as u64;
        if self.written >= MAX_LOG {
            bytes.extend_from_slice(b"[The rest of the output was not kept.]\n");
        }
        if f.write_all(&bytes).await.is_err() {
            self.file = None;
        }
    }
}

/// A phone notice about a deploy card. Notices about one app's deploys
/// replace each other; a tap opens the inbox, where the card is.
fn notice(c: &Card, head: &str, body: &str) -> Notice {
    Notice {
        tag: format!("deploy-{}", c.app_id),
        ..Notice::new(&c.app, head, body)
    }
}

/// Marks deploys a stopped hub left running as failed, then looks at every
/// app that deploys every [`POLL_SECS`] for as long as the hub runs.
pub fn watch(state: AppState) {
    tokio::spawn(async move {
        match state.store.recover_deploys().await {
            Ok(0) => {}
            Ok(n) => tracing::warn!(
                n,
                "deploys were running when the hub stopped; marked failed"
            ),
            Err(e) => tracing::error!(error = %e, "could not check for deploys left running"),
        }
        let Some(d) = state.deployer() else { return };
        let mut tick = tokio::time::interval(Duration::from_secs(POLL_SECS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            d.look_all().await;
        }
    });
}

// ---------- Routes ----------

fn no_deploys() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "This hub is not set up to deploy.",
    )
        .into_response()
}

/// Every deploy card, for the board and the phone.
pub(crate) async fn list(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("seeing deploys");
    }
    match st.store.deploy_cards().await {
        Ok(v) => Json(v).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// "Check for changes" on an app: looks at its main line now.
pub(crate) async fn check(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    UrlPath(app_id): UrlPath<i64>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("checking for changes to deploy");
    }
    let Some(d) = st.deployer() else {
        return no_deploys();
    };
    match st.store.deploy_rule(app_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return ApiError(StoreError::Refused(OFF.into())).into_response(),
        Err(e) => return ApiError(e).into_response(),
    }
    if let Err(why) = d.look(app_id).await {
        return (StatusCode::BAD_GATEWAY, why).into_response();
    }
    match st.store.deploy_cards().await {
        Ok(v) => Json(
            v.into_iter()
                .filter(|c| c.app_id == app_id)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct Go {
    /// The commit the card showed.
    commit: String,
    #[serde(default)]
    receipt: Option<String>,
}

/// The owner's Deploy (or Try again).
pub(crate) async fn go(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    UrlPath(id): UrlPath<i64>,
    Json(b): Json<Go>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("deploying");
    }
    let Some(d) = st.deployer() else {
        return no_deploys();
    };
    match st
        .store
        .start_deploy(id, &b.commit, b.receipt.as_deref())
        .await
    {
        Ok((card, fresh)) => {
            if fresh {
                tracing::info!(deploy = id, app = %card.app, commit = %card.short, "the owner tapped Deploy");
                tokio::spawn(async move { d.run(id).await });
            }
            Json(card).into_response()
        }
        Err(e) => ApiError(e).into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct Hide {
    #[serde(default)]
    receipt: Option<String>,
}

/// The owner's Not now.
pub(crate) async fn hide(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    UrlPath(id): UrlPath<i64>,
    Json(b): Json<Hide>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("hiding a deploy");
    }
    match st.store.hide_deploy(id, b.receipt.as_deref()).await {
        Ok(card) => Json(card).into_response(),
        Err(e) => ApiError(e).into_response(),
    }
}

/// A deploy's whole output, for Details.
pub(crate) async fn log(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
    UrlPath(id): UrlPath<i64>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("reading a deploy's output");
    }
    let card = match st.store.deploy_card(id).await {
        Ok(c) => c,
        Err(e) => return ApiError(e).into_response(),
    };
    let text = match st.deployer() {
        Some(d) => tokio::fs::read(d.log_path(id))
            .await
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned()),
        None => None,
    }
    .or(card.tail)
    .unwrap_or_else(|| "Nothing was printed yet.".into());
    (
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        text,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Step {
        v.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn files_the_steps_run_are_found_and_nothing_else() {
        let steps = vec![
            s(&["npm", "ci"]),
            s(&["npm", "ci", "--prefix", "infra"]),
            s(&["node", "scripts/deploy-prod.mjs", "--urgent"]),
            s(&["node", "./scripts/deploy-prod.mjs"]),
            s(&["npm", "run", "deploy:prod"]),
            s(&[
                "python",
                r"tools\ship.py",
                "../outside.py",
                "/etc/x.sh",
                "https://a.b/c.js",
            ]),
            s(&["bash", "release.sh", "v1.2", "--out=dist/x.js", "1.5"]),
        ];
        assert_eq!(
            named_files(&steps),
            vec!["release.sh", "scripts/deploy-prod.mjs", "tools/ship.py"]
        );
    }

    #[test]
    fn a_changed_or_missing_step_file_is_named() {
        let m = |v: &[(&str, &str)]| {
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect::<BTreeMap<_, _>>()
        };
        let before = m(&[("a.mjs", "1"), ("b.mjs", "2"), ("gone.sh", "3")]);
        let now = m(&[("a.mjs", "1"), ("b.mjs", "9"), ("new.sh", "4")]);
        assert_eq!(
            changed_files(&before, &now),
            vec!["b.mjs", "gone.sh", "new.sh"]
        );
        assert!(changed_files(&now, &now).is_empty());
    }

    #[test]
    fn script_headings_and_messy_output_are_read_plainly() {
        assert_eq!(
            heading("=== 2/5 Snapshot tmaa-1 ==="),
            Some("2/5 Snapshot tmaa-1".into())
        );
        assert_eq!(
            heading("\n=== 3/5 cdk deploy ===\r"),
            Some("3/5 cdk deploy".into())
        );
        assert_eq!(heading("=== ==="), None);
        assert_eq!(heading("> aws s3 sync"), None);
        assert_eq!(clean_line("\u{1b}[32mok\u{1b}[0m done\n"), "ok done");
        assert_eq!(clean_line("10%\r50%\r100% built\r\n"), "100% built");
        assert_eq!(
            clean_line("fetching https://bob:hunter2@github.com/acme/x.git"),
            scrub("fetching https://bob:hunter2@github.com/acme/x.git")
        );
        assert!(!clean_line("https://bob:hunter2@github.com/x").contains("hunter2"));
    }

    #[test]
    fn the_remotes_main_line_is_read_strictly() {
        let sha = "a".repeat(40);
        assert_eq!(
            read_symref(&format!("ref: refs/heads/main\tHEAD\n{sha}\tHEAD\n")),
            Some(("main".into(), sha.clone()))
        );
        assert_eq!(read_symref(&format!("{sha}\tHEAD\n")), None);
        assert_eq!(
            read_symref(&format!("ref: refs/heads/-x\tHEAD\n{sha}\tHEAD\n")),
            None
        );
        assert_eq!(
            read_symref("ref: refs/heads/main\tHEAD\nnot-a-sha\tHEAD\n"),
            None
        );
    }

    #[test]
    fn a_card_says_what_waits_in_plain_words() {
        let mut c = Card {
            id: 1,
            app_id: 1,
            app: "EMR".into(),
            state: "waiting".into(),
            commit: "abcdef1234".into(),
            short: "abcdef1".into(),
            branch: "main".into(),
            changes: vec!["Fix login".into(), "New price list".into()],
            released_before: true,
            steps: vec!["node scripts/deploy-prod.mjs".into()],
            steps_changed: vec![],
            site: None,
            step: None,
            error: None,
            tail: None,
            created_at: String::new(),
            started_at: None,
            ended_at: None,
            status: String::new(),
            line: String::new(),
            button: None,
        };
        assert_eq!(
            card_words(&c),
            (
                "Ready to deploy".into(),
                "2 changes since the last release".into(),
                Some("Deploy".into())
            )
        );
        c.changes.truncate(1);
        assert_eq!(card_words(&c).1, "1 change: Fix login");
        c.released_before = false;
        assert_eq!(card_words(&c).1, "Ready for its first deploy from Decurion");
        c.steps_changed = vec!["scripts/deploy-prod.mjs".into()];
        let (_, line, button) = card_words(&c);
        assert_eq!(line, "Deploy steps changed: scripts/deploy-prod.mjs");
        assert_eq!(button.as_deref(), Some("Deploy new steps"));
        c.state = "failed".into();
        c.error = Some("Step 3 of 3 stopped.".into());
        assert_eq!(card_words(&c).2.as_deref(), Some("Try again"));
        c.state = "live".into();
        assert_eq!(card_words(&c).1, "abcdef1 is live");
        for w in [
            card_words(&c).1,
            NEWER.into(),
            OFF.into(),
            INTERRUPTED.into(),
        ] {
            assert!(!w.contains('\u{2014}'), "no long dashes: {w}");
        }
    }

    #[test]
    fn steps_are_shown_as_written() {
        assert_eq!(
            shown(&s(&["node", "run.mjs", "my site"])),
            "node run.mjs \"my site\""
        );
    }
}
