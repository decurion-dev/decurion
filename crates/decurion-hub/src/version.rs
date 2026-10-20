//! Which Decurion this PC runs, and whether GitHub has a newer one.
//!
//! `GET /api/decurion/version` and the Decurion app's row on Today
//! ([`crate::today`]) both read the one answer made here, so they always
//! agree. The answer compares the commit this hub was built from
//! ([`crate::HUB_COMMIT`]) with the default branch of Decurion's own code as
//! its git server has it. Rules that must stay true:
//!
//! - **Only looks.** Nothing here updates, rebuilds or restarts Decurion; how
//!   to update is the owner's ([`crate::road::update_lines`]).
//! - **The server's tip, the walls of [`crate::upstream`].** The tip comes
//!   from [`crate::upstream::default_tip`]: the owner's folder is only ever
//!   read, the network is reached by remote name through [`crate::netgit`]
//!   with time limits, and no address, password or token is ever in an
//!   argument or a log. Counting the changes is local, read-only git in the
//!   hub's own copy.
//! - **Never holds a screen up for long.** One answer is kept for
//!   [`FRESH_SECS`]. An older one is still shown while a new look runs in
//!   the background, and only one look runs at a time. With no answer yet,
//!   the route waits at most [`WAIT_SECS`] and Today not at all.
//! - **Owner only.** The route is not in `cloud_reach`: a runner has no
//!   business knowing which Decurion the owner runs.

use crate::checkpoints::{git, is_commit};
use crate::{forbidden, AppState, Caller};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Serialize;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long one answer is used before a new look starts.
pub const FRESH_SECS: u64 = 600;

/// How long a look that could not reach the server is used before asking
/// again: a short outage should not hide a new Decurion for ten minutes.
const RETRY_SECS: u64 = 120;

/// The longest `GET /api/decurion/version` waits for a first answer.
const WAIT_SECS: u64 = 10;

/// How many waiting changes are named.
const MAX_CHANGES: usize = 8;

/// Whether this PC runs the newest Decurion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionState {
    /// The commit this hub was built from holds everything on the server.
    Current,
    /// The server has changes this hub was not built with.
    Behind,
    /// It could not be told; `words` says why.
    Unknown,
}

/// One change merged on the server that this PC is not running yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    /// The change in plain words, without its kind ("fix(hub): ") or its
    /// pull request number.
    pub title: String,
    /// Its pull request's number on GitHub, when the commit names one.
    pub number: Option<i64>,
}

/// The answer `GET /api/decurion/version` gives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Version {
    /// The full commit this hub was built from, when it knows it.
    pub commit: Option<String>,
    /// Its first seven characters.
    pub short: Option<String>,
    pub state: VersionState,
    /// Changes on the server that this PC is not running (merge commits of
    /// the default branch, counted once each).
    pub behind: i64,
    /// Up to [`MAX_CHANGES`] of them, newest first.
    pub changes: Vec<Change>,
    /// The app whose clone is Decurion's own code, when there is one.
    pub app_id: Option<i64>,
    /// When the server was last asked, once it was.
    pub checked_at: Option<String>,
    /// One plain sentence for the owner.
    pub words: String,
    /// How to update, in the owner's two ways.
    pub how: Vec<String>,
}

/// Why it could not be told whether this PC runs the newest Decurion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unknown {
    /// The hub was built without knowing its commit.
    NoCommit,
    /// No app's clone is Decurion's own code.
    NoApp,
    /// The git server could not be asked.
    Unreachable,
    /// The commit this hub was built from is not in Decurion's code.
    NotFound,
    /// The first look is still running.
    Checking,
}

/// The sentence for the owner.
pub fn words(state: VersionState, behind: i64, why: Option<Unknown>) -> String {
    match (state, why) {
        (VersionState::Current, _) => "Your PC runs the newest Decurion.".to_owned(),
        (VersionState::Behind, _) if behind == 1 => "1 change was merged on GitHub that your PC \
isn't running yet. Update Decurion to use it."
            .to_owned(),
        (VersionState::Behind, _) => format!(
            "{behind} changes were merged on GitHub that your PC isn't running yet. Update \
Decurion to use them."
        ),
        (VersionState::Unknown, Some(Unknown::NoApp)) => "Add Decurion's own code as an app, and \
Decurion can tell whether your PC runs the newest version."
            .to_owned(),
        (VersionState::Unknown, Some(Unknown::Unreachable)) => {
            "Couldn't reach GitHub to check.".to_owned()
        }
        (VersionState::Unknown, Some(Unknown::NotFound)) => "This Decurion was built from code \
that isn't on GitHub, so it can't tell whether it is the newest."
            .to_owned(),
        (VersionState::Unknown, Some(Unknown::Checking)) => {
            "Checking GitHub for a newer Decurion. Look again in a minute.".to_owned()
        }
        (VersionState::Unknown, Some(Unknown::NoCommit) | None) => {
            "This Decurion doesn't know which version it is.".to_owned()
        }
    }
}

/// One change from a commit's subject and body. A squash merge's subject
/// ends in its number ("fix(hub): a send ... (#51)"); GitHub's own merge
/// commit says "Merge pull request #51 from ..." and keeps the title in its
/// body. The kind before the colon is for programmers and is left out.
pub fn change(subject: &str, body: &str) -> Change {
    let subject = subject.trim();
    if let Some(rest) = subject.strip_prefix("Merge pull request #") {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse() {
            let title = body
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or(subject);
            return Change {
                title: plain_title(title),
                number: Some(n),
            };
        }
    }
    let mut title = subject;
    let mut number = None;
    if let Some(open) = subject.rfind(" (#") {
        let inside = &subject[open + 3..];
        if let Some(digits) = inside.strip_suffix(')') {
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                number = digits.parse().ok();
                title = subject[..open].trim_end();
            }
        }
    }
    Change {
        title: plain_title(title),
        number,
    }
}

/// `title` without a leading "kind(scope)!: ", starting with a capital.
fn plain_title(title: &str) -> String {
    let mut out = title.trim();
    if let Some((kind, rest)) = out.split_once(": ") {
        let name = kind.strip_suffix('!').unwrap_or(kind);
        let (word, scope) = match name.split_once('(') {
            Some((w, s)) => (w, Some(s)),
            None => (name, None),
        };
        let word_ok = !word.is_empty() && word.bytes().all(|b| b.is_ascii_lowercase());
        let scope_ok = scope.is_none_or(|s| s.ends_with(')') && !s[..s.len() - 1].contains(')'));
        if word_ok && scope_ok && !rest.trim().is_empty() {
            out = rest.trim();
        }
    }
    let mut chars = out.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The changes in `git log --format=%s%x1f%b%x1e`'s output, in its order.
fn changes_from(log: &str) -> Vec<Change> {
    log.split('\x1e')
        .map(|r| r.trim_start_matches(['\n', '\r']))
        .filter(|r| !r.trim().is_empty())
        .map(|r| {
            let (subject, body) = r.split_once('\x1f').unwrap_or((r, ""));
            change(subject, body)
        })
        .collect()
}

/// An answer that is not current or behind.
fn unknown(commit: Option<&str>, app_id: Option<i64>, why: Unknown) -> Version {
    Version {
        commit: commit.map(str::to_owned),
        short: commit.map(short),
        state: VersionState::Unknown,
        behind: 0,
        changes: Vec::new(),
        app_id,
        checked_at: None,
        words: words(VersionState::Unknown, 0, Some(why)),
        how: crate::road::update_lines(),
    }
}

fn short(commit: &str) -> String {
    commit.chars().take(7).collect()
}

/// The last answer and when it was made.
struct Kept {
    at: Instant,
    version: Version,
}

/// The one kept answer, and the lock that lets one look run at a time.
#[derive(Clone)]
pub struct Checks {
    /// The commit this hub was built from; tests give their own.
    commit: Arc<str>,
    kept: Arc<Mutex<Option<Kept>>>,
    looking: Arc<tokio::sync::Mutex<()>>,
    /// Added to the clock; only tests move it.
    skew: Arc<Mutex<Duration>>,
}

impl Default for Checks {
    fn default() -> Self {
        Self::for_commit(crate::HUB_COMMIT)
    }
}

impl Checks {
    /// Checks for a hub built from `commit`.
    pub fn for_commit(commit: &str) -> Self {
        Self {
            commit: commit.into(),
            kept: Arc::default(),
            looking: Arc::default(),
            skew: Arc::default(),
        }
    }

    fn now(&self) -> Instant {
        Instant::now() + *self.skew.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The kept answer, and whether it is still fresh.
    fn kept(&self) -> Option<(Version, bool)> {
        let kept = self.kept.lock().unwrap_or_else(|e| e.into_inner());
        let k = kept.as_ref()?;
        let keep = if k.version.state == VersionState::Unknown {
            RETRY_SECS
        } else {
            FRESH_SECS
        };
        let fresh = self.now().saturating_duration_since(k.at) < Duration::from_secs(keep);
        Some((k.version.clone(), fresh))
    }

    fn keep(&self, version: Version) {
        let at = self.now();
        *self.kept.lock().unwrap_or_else(|e| e.into_inner()) = Some(Kept { at, version });
    }
}

impl AppState {
    /// As if the hub were built from `commit` (for tests).
    #[doc(hidden)]
    pub fn with_hub_commit(mut self, commit: &str) -> Self {
        self.version = Checks::for_commit(commit);
        self
    }

    /// As if long enough had passed that the next look at the version
    /// starts a new one (for tests).
    #[doc(hidden)]
    pub fn age_version(&self) {
        *self.version.skew.lock().unwrap_or_else(|e| e.into_inner()) +=
            Duration::from_secs(FRESH_SECS + 1);
    }
}

/// The kept answer, starting a new look in the background when there is
/// none or it is old. With no answer yet, waits up to `wait` for the first
/// one; `None` when there is still none (Today then shows nothing).
pub(crate) async fn current(st: &AppState, wait: Option<Duration>) -> Option<Version> {
    let checks = &st.version;
    let kept = checks.kept();
    if let Some((v, true)) = &kept {
        return Some(v.clone());
    }
    if let Ok(guard) = checks.looking.clone().try_lock_owned() {
        let st = st.clone();
        tokio::spawn(async move {
            let v = look(&st).await;
            st.version.keep(v);
            drop(guard);
        });
    }
    if let Some((v, _)) = kept {
        return Some(v);
    }
    let wait = wait?;
    // The look holds the lock until its answer is kept.
    if tokio::time::timeout(wait, checks.looking.lock())
        .await
        .is_err()
    {
        return None;
    }
    checks.kept().map(|(v, _)| v)
}

/// Finds Decurion's own app, asks its git server for the tip of its default
/// branch, and counts what this hub was not built with.
async fn look(st: &AppState) -> Version {
    let commit = Some(st.version.commit.to_string()).filter(|c| is_commit(c));
    let Some(commit) = commit else {
        return unknown(None, None, Unknown::NoCommit);
    };
    let apps = match st.store.apps().await {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, "could not list apps to find Decurion's own code");
            return unknown(Some(&commit), None, Unknown::NoApp);
        }
    };
    let mut apps = apps;
    apps.sort_by_key(|a| a.id);
    let mut app = None;
    for a in apps {
        if crate::road::is_decurion(&a.repo_path).await {
            app = Some(a);
            break;
        }
    }
    let Some(app) = app else {
        return unknown(Some(&commit), None, Unknown::NoApp);
    };
    let Some(files) = st.files.clone() else {
        return unknown(Some(&commit), Some(app.id), Unknown::Unreachable);
    };
    let tip = match crate::upstream::default_tip(&files, app.id, &app.repo_path).await {
        Ok(tip) => tip,
        // New work is asking the same server: keep the last answer and
        // look again next time, rather than make that work wait.
        Err(why) if why == crate::upstream::TIP_BUSY => {
            return match st.version.kept() {
                Some((v, _)) => v,
                None => unknown(Some(&commit), Some(app.id), Unknown::Checking),
            };
        }
        Err(why) => {
            // Already scrubbed by default_tip.
            tracing::warn!(app = app.id, reason = %why, "could not ask GitHub for the newest Decurion");
            return unknown(Some(&commit), Some(app.id), Unknown::Unreachable);
        }
    };
    let checked_at =
        sqlx::query_scalar::<_, String>("SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now')")
            .fetch_one(st.store.pool())
            .await
            .ok();
    let cache = files.app_cache(app.id);
    let counted = {
        let _held = files.git_lock.lock().await;
        count(&cache, &commit, &tip).await
    };
    let Some((behind, changes)) = counted else {
        let mut v = unknown(Some(&commit), Some(app.id), Unknown::NotFound);
        v.checked_at = checked_at;
        return v;
    };
    let state = if behind > 0 {
        VersionState::Behind
    } else {
        VersionState::Current
    };
    Version {
        short: Some(short(&commit)),
        commit: Some(commit),
        state,
        behind,
        changes,
        app_id: Some(app.id),
        checked_at,
        words: words(state, behind, None),
        how: crate::road::update_lines(),
    }
}

/// How many first-parent commits `tip` has that `commit` lacks, and the
/// newest of them, read in the hub's copy `cache`. `None` when `commit` is
/// not there. Local and read-only.
async fn count(cache: &Path, commit: &str, tip: &str) -> Option<(i64, Vec<Change>)> {
    if !is_commit(commit) || !is_commit(tip) {
        return None;
    }
    git(cache, &["cat-file", "-e", &format!("{commit}^{{commit}}")])
        .await
        .ok()?;
    let range = format!("{commit}..{tip}");
    let behind: i64 = match git(cache, &["rev-list", "--first-parent", "--count", &range]).await {
        Ok(n) => n.trim().parse().ok()?,
        Err(e) => {
            tracing::warn!(error = %e, "could not count Decurion's new changes");
            return None;
        }
    };
    let max = format!("--max-count={MAX_CHANGES}");
    let log = git(
        cache,
        &[
            "log",
            "--first-parent",
            "--no-show-signature",
            "--no-color",
            &max,
            "--format=%s%x1f%b%x1e",
            &range,
            "--",
        ],
    )
    .await
    .unwrap_or_default();
    Some((behind, changes_from(&log)))
}

/// `GET /api/decurion/version`: owner only.
pub(crate) async fn get(State(st): State<AppState>, Extension(c): Extension<Caller>) -> Response {
    if c != Caller::Owner {
        return forbidden("seeing which Decurion this PC runs");
    }
    let commit = Some(st.version.commit.as_ref()).filter(|c| is_commit(c));
    let v = current(&st, Some(Duration::from_secs(WAIT_SECS)))
        .await
        .unwrap_or_else(|| {
            let app_id = None;
            match commit {
                Some(c) => unknown(Some(c), app_id, Unknown::Checking),
                None => unknown(None, app_id, Unknown::NoCommit),
            }
        });
    Json(v).into_response()
}

/// The Decurion app's version for its Today row: only when this PC is
/// behind, and never waiting on the network.
pub(crate) async fn for_today(st: &AppState) -> Option<Version> {
    current(st, None)
        .await
        .filter(|v| v.state == VersionState::Behind && v.app_id.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_squash_merges_title_and_number_are_read_from_its_subject() {
        assert_eq!(
            change(
                "feat(hub): Bring up to date merges main into Decurion's own pull requests on the owner's tap (#52)",
                ""
            ),
            Change {
                title: "Bring up to date merges main into Decurion's own pull requests on the owner's tap".into(),
                number: Some(52),
            }
        );
        assert_eq!(
            change("fix: a send no longer leaves a tracking ref (#51)", ""),
            Change {
                title: "A send no longer leaves a tracking ref".into(),
                number: Some(51),
            }
        );
        assert_eq!(
            change("feat(ui)!: one menu", ""),
            Change {
                title: "One menu".into(),
                number: None,
            }
        );
        // Not a kind, not a number: left as it is.
        assert_eq!(
            change("Note: keep this (#abc)", ""),
            Change {
                title: "Note: keep this (#abc)".into(),
                number: None,
            }
        );
        assert_eq!(
            change("Update README.md", "").title,
            "Update README.md".to_owned()
        );
    }

    #[test]
    fn githubs_merge_commit_takes_its_title_from_the_body() {
        assert_eq!(
            change(
                "Merge pull request #48 from decurion-dev/claude/x",
                "\nfeat(release): start at sign-in\n\nmore words"
            ),
            Change {
                title: "Start at sign-in".into(),
                number: Some(48),
            }
        );
        assert_eq!(change("Merge pull request #7 from a/b", "").number, Some(7));
    }

    #[test]
    fn a_log_is_read_one_change_at_a_time() {
        let log = "fix: b (#2)\x1f\x1e\nMerge pull request #1 from x/y\x1fa title\n\x1e\n";
        assert_eq!(
            changes_from(log),
            vec![
                Change {
                    title: "B".into(),
                    number: Some(2)
                },
                Change {
                    title: "A title".into(),
                    number: Some(1)
                },
            ]
        );
        assert!(changes_from("").is_empty());
    }

    #[test]
    fn the_words_say_where_this_pc_stands() {
        assert_eq!(
            words(VersionState::Current, 0, None),
            "Your PC runs the newest Decurion."
        );
        assert_eq!(
            words(VersionState::Behind, 2, None),
            "2 changes were merged on GitHub that your PC isn't running yet. Update Decurion to use them."
        );
        assert_eq!(
            words(VersionState::Behind, 1, None),
            "1 change was merged on GitHub that your PC isn't running yet. Update Decurion to use it."
        );
        assert_eq!(
            words(VersionState::Unknown, 0, Some(Unknown::NoCommit)),
            "This Decurion doesn't know which version it is."
        );
        assert_eq!(
            words(VersionState::Unknown, 0, Some(Unknown::Unreachable)),
            "Couldn't reach GitHub to check."
        );
    }

    #[test]
    fn an_unknown_answer_still_says_how_to_update() {
        let v = unknown(None, None, Unknown::NoCommit);
        assert_eq!(v.commit, None);
        assert_eq!(v.short, None);
        assert_eq!(v.how, crate::road::update_lines());
        let json = serde_json::to_value(&v).expect("json");
        assert_eq!(json["state"], "unknown");
        assert_eq!(json["commit"], serde_json::Value::Null);
    }
}
