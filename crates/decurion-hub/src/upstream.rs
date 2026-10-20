//! Where new work starts: the app's default branch as its git server has it,
//! never whatever branch the owner's folder happens to be on.
//!
//! Rules that must stay true:
//!
//! - **The server says, not the folder's memory.** The default branch comes
//!   from `git ls-remote --symref origin HEAD` run in the owner's folder (the
//!   remote by name, read-only, with the owner's own sign-in). The folder's
//!   saved `origin/HEAD` and its local branches are never trusted for it.
//!   The owner's folder is only ever read.
//! - **The server's answer lands in a copy of its own.** That branch is
//!   fetched by the owner's folder, by remote name, exactly as the owner's
//!   own `git fetch` would reach the server, but with every new object sent
//!   to a bare repo used for nothing else ([`Files::upstream_cache`],
//!   `app-<id>-upstream.git`) and no ref, `FETCH_HEAD` or object written in
//!   the folder ([`fetch_command`]). That copy borrows the objects of the
//!   hub's copy of the app ([`Files::app_cache`]) through
//!   `objects/info/alternates`, and copies in, locally, whatever the fetch
//!   found already in the folder. The hub's copy then takes the one branch
//!   from it with a local fetch, under `git_lock`. Nothing from the network
//!   writes the hub's copy directly.
//! - **No secrets in arguments or logs.** Both network steps name the
//!   remote, `origin`, never its address, so a token in the folder's
//!   address, its credential helper, extra headers, proxy and address
//!   rewrites all apply as the folder has them and none is ever on a
//!   command line. Every git message is passed through [`scrub`] before it
//!   is logged.
//! - **Never waits on a question, never for long.** Both network steps run
//!   through [`netgit`], each with a time limit of [`NET_SECS`]. The
//!   server's own answer is used again for [`REUSE_SECS`] for the same
//!   folder and address, so requests that come together wait on it once.
//! - **Network work holds no lock other work needs.** It runs outside
//!   `git_lock`, under a lock of the app's own ([`Locks`]), so two requests
//!   for one app do not ask the server twice. Reading the folder, importing
//!   the one branch, choosing and pinning hold `git_lock`, and are local.
//!   A server that answered but whose answer could not be taken in counts
//!   as one that did not answer.
//! - **A start never disappears.** The chosen commit is pinned at
//!   `refs/decurion/start/<sha>` in the hub's copy before anything uses it.
//! - **Falls back in order, and says so once.** With no `origin`, or a
//!   server that cannot be reached, is slow or names no default: the default
//!   it gave last time for this same folder and address
//!   (`refs/upstream-default/<id>`, keyed by [`identity`], so a changed
//!   folder never inherits the old one's); else the folder's own `main` or
//!   `master`, as the hub's copy read it after the wait; else whatever the
//!   folder has out, which is how Decurion chose before. A fallback is logged once per folder and address, scrubbed,
//!   until the server answers again.

use crate::checkpoints::{git, io, is_commit, Files};
use crate::github::scrub;
use crate::netgit;
use crate::protected::plain_branch;
use crate::store::{Result, StoreError};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

/// The longest each network step (asking, then fetching) may take. A first
/// fetch of a large app can take a while even though it only brings what
/// the folder does not already have.
pub(crate) const NET_SECS: u64 = 120;

/// How long the server's own answer is used again for the same folder and
/// address, without asking: a goal, its approval and new work started
/// together wait on the network once, not each time.
pub(crate) const REUSE_SECS: u64 = 60;

/// An app's last start, and when it was ready.
struct Last {
    ready: Instant,
    sha: String,
    /// The folder and address ([`identity`]) the server itself gave this
    /// start for. `None` for a fallback, which is never reused past the
    /// requests that waited for it.
    server: Option<String>,
}

/// One app's lock for asking its git server, holding its last start.
type AppLock = Arc<tokio::sync::Mutex<Option<Last>>>;

/// One lock per app for asking its git server, so a request that waited
/// behind another one asking at the same moment takes that answer instead
/// of asking again; and which folders' fallbacks were already logged.
#[derive(Default)]
pub(crate) struct Locks {
    apps: Mutex<HashMap<i64, AppLock>>,
    warned: Mutex<HashSet<String>>,
    /// Added to the clock; only tests move it.
    skew: Mutex<Duration>,
}

impl Locks {
    fn now(&self) -> Instant {
        Instant::now() + *self.skew.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Moves this hub's clock for reusing answers forward by `d`.
    pub(crate) fn pass(&self, d: Duration) {
        *self.skew.lock().unwrap_or_else(|e| e.into_inner()) += d;
    }

    fn for_app(&self, app_id: i64) -> AppLock {
        // A poisoned map only means another request panicked; the locks in
        // it are still good.
        let mut map = self.apps.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(app_id).or_default().clone()
    }

    /// True the first time the server for folder-and-address `id` could not
    /// be asked since it last answered, so a long outage is logged once.
    pub(crate) fn first_fallback(&self, id: &str) -> bool {
        let mut warned = self.warned.lock().unwrap_or_else(|e| e.into_inner());
        warned.insert(id.to_owned())
    }

    /// The server for `id` answered: its next fallback is news again.
    pub(crate) fn answered(&self, id: &str) {
        let mut warned = self.warned.lock().unwrap_or_else(|e| e.into_inner());
        warned.remove(id);
    }
}

/// The commit new work in this app starts from, pinned in the hub's copy.
/// Fails only when even the folder's own current commit cannot be read.
pub(crate) async fn start_commit(files: &Files, app_id: i64, repo_path: &str) -> Result<String> {
    let clock = &files.upstream;
    let asked_at = clock.now();
    let lock = clock.for_app(app_id);
    let mut last = lock.lock().await;
    if let Some(l) = last.as_ref() {
        if l.ready >= asked_at {
            return Ok(l.sha.clone());
        }
    }
    let url = git(Path::new(repo_path), &["remote", "get-url", "origin"])
        .await
        .ok()
        .filter(|u| !u.is_empty());
    let id = identity(
        repo_path,
        &url.as_deref().map(credential_free).unwrap_or_default(),
    );
    if let Some(l) = last.as_ref() {
        let age = clock.now().saturating_duration_since(l.ready);
        if age < Duration::from_secs(REUSE_SECS) && l.server.as_deref() == Some(id.as_str()) {
            return Ok(l.sha.clone());
        }
    }
    let (sha, from_server) = resolve(files, app_id, repo_path, url.is_some(), &id).await?;
    *last = Some(Last {
        ready: clock.now(),
        sha: sha.clone(),
        server: from_server.then_some(id),
    });
    Ok(sha)
}

/// The start for a goal about to be created, chosen before its write
/// transaction opens. `None` when this hub keeps no saved work, or not even
/// the folder's own commit can be read; that goal's helpers then copy the
/// folder as before, and fail there the way they always would have.
pub(crate) async fn for_new_goal(st: &crate::AppState, app_id: i64) -> Option<String> {
    let files = st.files.clone()?;
    let app = st.store.app(app_id).await.ok()?;
    match start_commit(&files, app.id, &app.repo_path).await {
        Ok(sha) => Some(sha),
        Err(e) => {
            tracing::warn!(app = app_id, error = %scrub(&e.to_string()), "no start commit for a goal");
            None
        }
    }
}

/// A fresh start for the new pieces of goal `goal`, when approving
/// `plan_message_id` would add pieces to a goal that already has some, or
/// to one with no start yet. `None` otherwise: the first approval keeps the
/// start the goal was created with, and a request that would be refused
/// never waits on the network.
pub(crate) async fn for_next_round(
    st: &crate::AppState,
    goal: i64,
    plan_message_id: i64,
) -> Option<String> {
    match st.store.next_round(goal, plan_message_id).await {
        Ok(Some(app_id)) => for_new_goal(st, app_id).await,
        _ => None,
    }
}

/// The tip of `branch` on the app's git server, brought into the hub's copy
/// and pinned at `refs/decurion/merge/<sha>`, for Bring up to date
/// ([`crate::pr_watch`]): the newest commit of the branch a pull request
/// merges into. The same network path as [`start_commit`] (the folder's
/// `origin` by name, read-only, into the server copy first), but with no
/// fallback: merging an old copy of the branch would not bring the pull
/// request up to date, so when the server cannot be asked this refuses, in
/// plain words, and nothing is started.
pub(crate) async fn branch_tip(
    files: &Files,
    app_id: i64,
    repo_path: &str,
    branch: &str,
) -> Result<String> {
    let refused = || {
        StoreError::Refused(format!(
            "Decurion could not get the newest {branch} from the app's git server, so nothing \
was started. Check this PC's internet connection, then tap Bring up to date again."
        ))
    };
    if !plain_branch(branch) {
        return Err(refused());
    }
    let lock = files.upstream.for_app(app_id);
    let _one = lock.lock().await;
    let folder = Path::new(repo_path);
    let Some(url) = git(folder, &["remote", "get-url", "origin"])
        .await
        .ok()
        .filter(|u| !u.is_empty())
    else {
        tracing::warn!(
            app = app_id,
            "no origin to bring a pull request up to date from"
        );
        return Err(refused());
    };
    let id = identity(repo_path, &credential_free(&url));
    let cache = files.app_cache(app_id);
    let asked = async {
        {
            let _held = files.git_lock.lock().await;
            files
                .fetch_app(&cache, repo_path)
                .await
                .map_err(|e| failed(&e))?;
        }
        let limit = Duration::from_secs(NET_SECS);
        let out = run(ls_remote_branch_command(folder, branch).await, limit).await?;
        let sha = advertised_branch(&String::from_utf8_lossy(&out.stdout), branch)
            .ok_or_else(|| NotAsked::Failed(format!("the server has no branch {branch}")))?;
        bring(files, app_id, folder, &id, branch, &sha).await?;
        let _held = files.git_lock.lock().await;
        let got = take_branch(files, app_id, &cache, &id, branch)
            .await
            .map_err(|e| failed(&e))?;
        if got != sha || !is_commit(&got) {
            return Err(NotAsked::Failed(
                "the branch moved while it was fetched".into(),
            ));
        }
        git(
            &cache,
            &["update-ref", &format!("refs/decurion/merge/{got}"), &got],
        )
        .await
        .map_err(|e| failed(&e))?;
        Ok(got)
    }
    .await;
    asked.map_err(|why| {
        tracing::warn!(
            app = app_id,
            reason = %scrub(&why.to_string()),
            "could not get the base branch to bring a pull request up to date"
        );
        refused()
    })
}

/// What [`default_tip`] answers when the app's server is being asked for
/// new work right now.
pub(crate) const TIP_BUSY: &str = "the app's git server is being asked for new work right now";

/// The tip of the app's default branch as its git server has it now, in the
/// hub's copy, for telling whether the running Decurion is the newest
/// ([`crate::version`]). The same network path as [`start_commit`] (the
/// folder's `origin` by name, read-only, into the server copy first, each
/// step within [`NET_SECS`]), but with no fallback: an old copy of the
/// branch would say Decurion is up to date when it may not be. The error is
/// a reason for the log, already scrubbed, never for the owner's screen.
/// It never waits for the app's lock: new work starting on the app holds
/// it, and a look at the version must not make that work wait. Busy, it
/// answers [`TIP_BUSY`] and the caller keeps what it knew.
pub(crate) async fn default_tip(
    files: &Files,
    app_id: i64,
    repo_path: &str,
) -> std::result::Result<String, String> {
    let lock = files.upstream.for_app(app_id);
    let Ok(_one) = lock.try_lock() else {
        return Err(TIP_BUSY.to_owned());
    };
    let folder = Path::new(repo_path);
    let Some(url) = git(folder, &["remote", "get-url", "origin"])
        .await
        .ok()
        .filter(|u| !u.is_empty())
    else {
        return Err(NotAsked::NoOrigin.to_string());
    };
    let id = identity(repo_path, &credential_free(&url));
    let cache = files.app_cache(app_id);
    let asked = async {
        {
            let _held = files.git_lock.lock().await;
            files
                .fetch_app(&cache, repo_path)
                .await
                .map_err(|e| failed(&e))?;
        }
        let branch = ask_server(files, app_id, folder, &id).await?;
        let _held = files.git_lock.lock().await;
        let sha = take_default(files, app_id, &cache, &id, &branch)
            .await
            .map_err(|e| failed(&e))?;
        if !is_commit(&sha) {
            return Err(NotAsked::Failed(format!("not a commit id: {sha}")));
        }
        Ok(sha)
    }
    .await;
    match asked {
        Ok(sha) => {
            files.upstream.answered(&id);
            Ok(sha)
        }
        Err(why) => Err(scrub(&why.to_string())),
    }
}

/// Asks the folder's `origin`, by name, where one branch is. Read-only.
async fn ls_remote_branch_command(folder: &Path, branch: &str) -> tokio::process::Command {
    let mut cmd = netgit::command(folder).await;
    cmd.args(["ls-remote", "origin", &format!("refs/heads/{branch}")]);
    cmd
}

/// The commit `branch` is at in `git ls-remote <remote> refs/heads/<branch>`'s
/// answer: only that exact name, and only a full commit id.
pub(crate) fn advertised_branch(ls_remote: &str, branch: &str) -> Option<String> {
    let want = format!("refs/heads/{branch}");
    ls_remote.lines().find_map(|line| {
        let (sha, name) = line.split_once('\t')?;
        (name.trim() == want && is_commit(sha)).then(|| sha.to_owned())
    })
}

/// Why the server's default could not be used this time.
enum NotAsked {
    NoOrigin,
    TooSlow,
    NoGit(std::io::Error),
    /// git's own message, already scrubbed.
    Failed(String),
    NoDefault,
}

impl std::fmt::Display for NotAsked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoOrigin => f.write_str("the folder has no remote named origin"),
            Self::TooSlow => write!(f, "the server did not answer within {NET_SECS} seconds"),
            Self::NoGit(e) => write!(f, "git could not be started: {e}"),
            Self::Failed(e) => write!(f, "git said: {e}"),
            Self::NoDefault => f.write_str("the server named no default branch"),
        }
    }
}

/// The start, and whether the server itself gave it. `id` is the folder
/// and its `origin` address ([`identity`]); `has_origin` says there is one.
async fn resolve(
    files: &Files,
    app_id: i64,
    repo_path: &str,
    has_origin: bool,
    id: &str,
) -> Result<(String, bool)> {
    let folder = Path::new(repo_path);
    let cache = files.app_cache(app_id);
    // The folder's own branches first, locally, so the fallbacks below have
    // something to choose from even if this is the app's first work.
    let read_folder = {
        let _held = files.git_lock.lock().await;
        files.fetch_app(&cache, repo_path).await
    };
    let asked = match (has_origin, &read_folder) {
        (false, _) => Err(NotAsked::NoOrigin),
        (true, Err(e)) => Err(failed(e)),
        (true, Ok(())) => ask_server(files, app_id, folder, id).await,
    };
    let _held = files.git_lock.lock().await;
    // A server that answered but whose answer could not be taken in is
    // treated like one that did not answer: the fallbacks still give work a
    // start.
    let from_server = match asked {
        Ok(branch) => take_default(files, app_id, &cache, id, &branch)
            .await
            .map_err(|e| failed(&e)),
        Err(why) => Err(why),
    };
    let answered = from_server.is_ok();
    let sha = match from_server {
        Ok(sha) => {
            files.upstream.answered(id);
            sha
        }
        Err(why) => {
            // The folder again, now: it may have moved during the minutes
            // the server took, and only what the hub's copy holds can be
            // pinned.
            files.fetch_app(&cache, repo_path).await?;
            let (sha, used) = fall_back(&cache, folder, id).await?;
            if files.upstream.first_fallback(id) {
                tracing::warn!(
                    app = app_id,
                    reason = %scrub(&why.to_string()),
                    used,
                    "could not ask the app's git server where new work starts"
                );
            }
            sha
        }
    };
    if !is_commit(&sha) {
        return Err(StoreError::Invalid(format!("not a commit id: {sha}")));
    }
    git(
        &cache,
        &["update-ref", &format!("refs/decurion/start/{sha}"), &sha],
    )
    .await?;
    Ok((sha, answered))
}

/// A failed local step, as a reason to fall back, scrubbed.
fn failed(e: &StoreError) -> NotAsked {
    NotAsked::Failed(scrub(&e.to_string()))
}

/// Takes the server's default branch, already in the app's server copy as
/// `refs/upstream/<id>/<branch>`, into the hub's copy with a local fetch,
/// and saves it as this folder's last known default. Returns its commit.
/// The caller holds `git_lock`.
async fn take_default(
    files: &Files,
    app_id: i64,
    cache: &Path,
    id: &str,
    branch: &str,
) -> Result<String> {
    let sha = take_branch(files, app_id, cache, id, branch).await?;
    git(
        cache,
        &["update-ref", &format!("refs/upstream-default/{id}"), &sha],
    )
    .await?;
    Ok(sha)
}

/// Takes `refs/upstream/<id>/<branch>` from the app's server copy into the
/// hub's copy with a local fetch. Returns its commit. The caller holds
/// `git_lock`.
async fn take_branch(
    files: &Files,
    app_id: i64,
    cache: &Path,
    id: &str,
    branch: &str,
) -> Result<String> {
    let from = files.upstream_cache(app_id).to_string_lossy().into_owned();
    let name = format!("refs/upstream/{id}/{branch}");
    git(
        cache,
        &[
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.file.allow=always",
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--",
            &from,
            &format!("+{name}:{name}"),
        ],
    )
    .await?;
    git(
        cache,
        &["rev-parse", "--verify", &format!("{name}^{{commit}}")],
    )
    .await
}

/// Asks the server its default branch and brings that branch's commit into
/// the app's server copy, as `refs/upstream/<id>/<branch>`. Returns the
/// branch name. The caller holds the app's own lock, which is all the
/// server copy needs.
async fn ask_server(
    files: &Files,
    app_id: i64,
    folder: &Path,
    id: &str,
) -> std::result::Result<String, NotAsked> {
    let limit = Duration::from_secs(NET_SECS);
    let out = run(ls_remote_command(folder).await, limit).await?;
    let (branch, sha) =
        advertised_default(&String::from_utf8_lossy(&out.stdout)).ok_or(NotAsked::NoDefault)?;
    bring(files, app_id, folder, id, &branch, &sha).await?;
    Ok(branch)
}

/// Brings commit `sha`, which the server says `branch` is at, into the
/// app's server copy as `refs/upstream/<id>/<branch>`. The caller holds the
/// app's own lock.
async fn bring(
    files: &Files,
    app_id: i64,
    folder: &Path,
    id: &str,
    branch: &str,
    sha: &str,
) -> std::result::Result<(), NotAsked> {
    let limit = Duration::from_secs(NET_SECS);
    let up = files.upstream_cache(app_id);
    let cache = files.app_cache(app_id);
    files
        .init_upstream_cache(&up, &cache)
        .await
        .map_err(|e| failed(&e))?;
    let own = folder_objects(folder).await.map_err(|e| failed(&e))?;
    let cmd = fetch_command(folder, &up, &own, branch)
        .await
        .map_err(|e| failed(&e))?;
    run(cmd, limit).await?;
    keep_what_it_needs(&up, &cache, &own, sha)
        .await
        .map_err(|e| failed(&e))?;
    git(&up, &["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .await
        .map_err(|_| NotAsked::Failed("the server did not send the commit it named".into()))?;
    git(
        &up,
        &["update-ref", &format!("refs/upstream/{id}/{branch}"), sha],
    )
    .await
    .map_err(|e| failed(&e))?;
    Ok(())
}

async fn run(
    cmd: tokio::process::Command,
    limit: Duration,
) -> std::result::Result<std::process::Output, NotAsked> {
    match netgit::output(cmd, limit).await {
        Err(netgit::Failed::TooSlow) => Err(NotAsked::TooSlow),
        Err(netgit::Failed::NotStarted(e)) => Err(NotAsked::NoGit(e)),
        Ok(out) if !out.status.success() => Err(NotAsked::Failed(scrub(
            String::from_utf8_lossy(&out.stderr).trim(),
        ))),
        Ok(out) => Ok(out),
    }
}

/// Asks the folder's `origin`, by name, which branch is its default and
/// where it is. Read-only.
async fn ls_remote_command(folder: &Path) -> tokio::process::Command {
    let mut cmd = netgit::command(folder).await;
    cmd.args(["ls-remote", "--symref", "origin", "HEAD"]);
    cmd
}

/// The fetch of the server's default branch, run in the owner's folder so
/// it reaches the server exactly as the owner's own `git fetch` does: the
/// remote by name, with the folder's own address, sign-in, proxy, extra
/// headers and address rewrites, none of them ever on the command line.
/// It writes nothing in the folder: no ref (one exact source, no
/// destination, no mappings, no pruning, no tags), no `FETCH_HEAD`, and its
/// new objects go to the server copy `up` ([`fetch_args`]), with the
/// folder's objects `own` only borrowed for the fetch.
pub(crate) async fn fetch_command(
    folder: &Path,
    up: &Path,
    own: &Path,
    branch: &str,
) -> Result<tokio::process::Command> {
    let into = std::path::absolute(up.join("objects")).map_err(io)?;
    let mut cmd = netgit::command(folder).await;
    cmd.env("GIT_OBJECT_DIRECTORY", into)
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", own)
        .args(fetch_args(branch));
    Ok(cmd)
}

/// The owner's folder's own object store.
async fn folder_objects(folder: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(
        git(
            folder,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "objects",
            ],
        )
        .await?,
    ))
}

/// Packs into the server copy `up` whatever commit `sha` needs that neither
/// it nor the hub's copy `cache` holds. The fetch borrowed the owner's
/// folder: git does not ask the server for what the folder already has
/// (often everything, when the folder fetched lately), and the folder is
/// only ever read, so those objects are copied here from it, locally.
/// What either copy's branches already reach is left out, and nothing is
/// written when that is everything, so asking again at the same commit
/// adds nothing.
async fn keep_what_it_needs(up: &Path, cache: &Path, own: &Path, sha: &str) -> Result<()> {
    // `^tip` lines, not a `--not` line: git before 2.42 refuses any option
    // on standard input.
    let mut revs = format!("{sha}\n");
    for repo in [cache, up] {
        for tip in git(repo, &["for-each-ref", "--format=%(objectname)"])
            .await?
            .lines()
            .filter(|l| !l.is_empty())
        {
            revs.push_str(&format!("^{tip}\n"));
        }
    }
    let missing = with_input(up, own, &["rev-list", "--stdin", "--count"], &revs).await?;
    if missing.trim() == "0" {
        return Ok(());
    }
    let pack = std::path::absolute(up.join("objects").join("pack").join("pack")).map_err(io)?;
    let pack = pack.to_string_lossy().into_owned();
    with_input(
        up,
        own,
        &["pack-objects", "--revs", "--quiet", &pack],
        &revs,
    )
    .await?;
    Ok(())
}

/// git in the server copy `up`, borrowing the owner's folder's objects
/// `own`, with `input` on its standard input. Returns what it printed.
async fn with_input(up: &Path, own: &Path, args: &[&str], input: &str) -> Result<String> {
    let mut child = tokio::process::Command::new("git")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", own)
        .arg("-C")
        .arg(up)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(io)?;
    let what = args.first().copied().unwrap_or("");
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| StoreError::Invalid(format!("git {what}: no input")))?;
    stdin.write_all(input.as_bytes()).await.map_err(io)?;
    drop(stdin);
    let out = child.wait_with_output().await.map_err(io)?;
    if !out.status.success() {
        return Err(StoreError::Invalid(format!(
            "git {what}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The start when the server's default cannot be had, and which fallback
/// gave it. The caller holds `git_lock` and has just read the folder into
/// the hub's copy, so every commit this returns is there to pin.
async fn fall_back(cache: &Path, folder: &Path, id: &str) -> Result<(String, &'static str)> {
    if let Ok(sha) = git(
        cache,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/upstream-default/{id}^{{commit}}"),
        ],
    )
    .await
    {
        return Ok((sha, "the default the server gave last time"));
    }
    // The hub's copy of the folder's branch, and only for a branch the
    // folder has: the copy may still hold branches of a folder the app
    // used before.
    for (b, used) in [
        ("main", "the folder's main branch"),
        ("master", "the folder's master branch"),
    ] {
        let has = git(
            folder,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{b}"),
            ],
        )
        .await
        .is_ok();
        if !has {
            continue;
        }
        if let Ok(sha) = git(
            cache,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/remotes/app/{b}^{{commit}}"),
            ],
        )
        .await
        {
            return Ok((sha, used));
        }
    }
    Ok((
        git(cache, &["rev-parse", "refs/heads/decurion-latest"]).await?,
        "the folder's current branch",
    ))
}

/// The default branch and its commit in `git ls-remote --symref <remote>
/// HEAD`'s answer, if it names a plain branch and a full commit id.
pub(crate) fn advertised_default(ls_remote: &str) -> Option<(String, String)> {
    let mut branch = None;
    let mut sha = None;
    for line in ls_remote.lines() {
        let Some((target, name)) = line.split_once('\t') else {
            continue;
        };
        if name.trim() != "HEAD" {
            continue;
        }
        match target.strip_prefix("ref: ") {
            Some(r) => {
                let b = r.strip_prefix("refs/heads/")?;
                if !plain_branch(b) {
                    return None;
                }
                branch = Some(b.to_owned());
            }
            None if is_commit(target) => sha = Some(target.to_owned()),
            None => {}
        }
    }
    Some((branch?, sha?))
}

/// git's arguments for fetching the server's default branch, after what
/// [`netgit::command`] puts first. The remote by name, never an address;
/// exactly one source and no destination, none of the remote's own
/// mappings, no pruning (which could also take tags), no tags, no
/// submodules, and no upkeep that would write in the owner's folder. Pure,
/// so a test can check no secret is ever among them.
pub(crate) fn fetch_args(branch: &str) -> Vec<String> {
    [
        "-c",
        "gc.auto=0",
        "-c",
        "maintenance.auto=false",
        "-c",
        "fetch.writeCommitGraph=false",
        "-c",
        "core.fsmonitor=false",
        "fetch",
        "--quiet",
        "--no-tags",
        "--no-recurse-submodules",
        "--no-write-fetch-head",
        "--no-prune",
        "--refmap=",
        "--",
        "origin",
        &format!("refs/heads/{branch}"),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// `url` with any password taken out, for [`identity`]. The user name
/// stays; a user name that is itself a token (GitHub or GitLab token marks,
/// or a long opaque string) goes too, so a new token does not make the
/// same server look like another one.
pub(crate) fn credential_free(url: &str) -> String {
    let Some(i) = url.find("://") else {
        return url.to_owned();
    };
    let (scheme, rest) = (&url[..i], &url[i + 3..]);
    let end = rest.find('/').unwrap_or(rest.len());
    let (auth, path) = rest.split_at(end);
    let Some(at) = auth.rfind('@') else {
        return url.to_owned();
    };
    let (user, host) = (&auth[..at], &auth[at + 1..]);
    let login = match user.split(':').next() {
        Some(name) if !name.is_empty() && !looks_like_token(name) => format!("{name}@"),
        _ => String::new(),
    };
    format!("{scheme}://{login}{host}{path}")
}

fn looks_like_token(name: &str) -> bool {
    const MARKS: &[&str] = &[
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "glpat-",
    ];
    MARKS.iter().any(|m| name.starts_with(m)) || name.len() >= 40
}

/// A short name for one folder and one server address together, so the
/// default saved for one is never used for another.
pub(crate) fn identity(repo_path: &str, url: &str) -> String {
    let folder = std::fs::canonicalize(repo_path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| repo_path.to_owned());
    let digest = Sha256::digest(format!("{folder}\n{url}").as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

impl Files {
    /// As if long enough had passed that the next start asks the git
    /// server again rather than use its last answer (for tests).
    #[doc(hidden)]
    pub fn age_answers(&self) {
        self.upstream.pass(Duration::from_secs(REUSE_SECS + 1));
    }

    /// True if `sha` is a start this hub pinned for app `app_id`.
    pub(crate) async fn is_pinned_start(&self, app_id: i64, sha: &str) -> bool {
        is_commit(sha)
            && git(
                &self.app_cache(app_id),
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/decurion/start/{sha}^{{commit}}"),
                ],
            )
            .await
            .is_ok()
    }

    /// True if `sha` is a base-branch commit this hub pinned for app
    /// `app_id` for Bring up to date ([`branch_tip`]). Kept apart from the
    /// starts, so it never counts as one.
    pub(crate) async fn is_pinned_merge(&self, app_id: i64, sha: &str) -> bool {
        is_commit(sha)
            && git(
                &self.app_cache(app_id),
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/decurion/merge/{sha}^{{commit}}"),
                ],
            )
            .await
            .is_ok()
    }

    /// The bare repo that only takes fetches from the app's git server.
    pub(crate) fn upstream_cache(&self, app_id: i64) -> PathBuf {
        self.app_cache(app_id)
            .with_file_name(format!("app-{app_id}-upstream.git"))
    }

    /// Makes the app's server copy if needed and has it borrow the objects
    /// of the hub's copy `cache`, so the server is only asked for what is
    /// new. The caller holds the app's own lock.
    pub(crate) async fn init_upstream_cache(&self, up: &Path, cache: &Path) -> Result<()> {
        if !up.join("objects").is_dir() {
            std::fs::create_dir_all(up).map_err(io)?;
            git(up, &["init", "--quiet", "--bare"]).await?;
        }
        let objects = std::path::absolute(cache.join("objects")).map_err(io)?;
        // Forward slashes: git reads them on every system.
        let line = format!("{}\n", objects.to_string_lossy().replace('\\', "/"));
        let info = up.join("objects").join("info");
        std::fs::create_dir_all(&info).map_err(io)?;
        let file = info.join("alternates");
        if std::fs::read_to_string(&file).ok().as_deref() != Some(line.as_str()) {
            std::fs::write(&file, line).map_err(io)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up `user:password` for addresses in these tests, built from
    /// parts so the source itself holds no address with a login in it.
    fn login(user: &str, password: &str) -> String {
        [user, password].join(":")
    }

    /// git for the tests' own repos, with a fixed author. Returns its output.
    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=trunk",
            ])
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// A server ("origin", default branch `trunk`) fed from `seed`, and the
    /// owner's folder cloned from it.
    struct Repos {
        root: PathBuf,
        seed: PathBuf,
        origin: PathBuf,
        folder: PathBuf,
    }

    impl Repos {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("decurion-up-{}", uuid::Uuid::new_v4()));
            let seed = root.join("seed");
            std::fs::create_dir_all(&seed).expect("seed dir");
            sh(&seed, &["init", "--quiet"]);
            let r = Self {
                origin: root.join("origin.git"),
                folder: root.join("app"),
                seed,
                root,
            };
            r.commit("a");
            let (seed, origin) = (r.seed.to_string_lossy(), r.origin.to_string_lossy());
            sh(&r.root, &["clone", "--quiet", "--bare", &seed, &origin]);
            sh(
                &r.root,
                &["clone", "--quiet", &origin, &r.folder.to_string_lossy()],
            );
            r
        }

        fn commit(&self, text: &str) -> String {
            std::fs::write(self.seed.join("README.md"), text).expect("write");
            sh(&self.seed, &["add", "."]);
            sh(&self.seed, &["commit", "--quiet", "-m", text]);
            sh(&self.seed, &["rev-parse", "HEAD"])
        }

        /// A new commit on the server's default branch.
        fn advance(&self, text: &str) -> String {
            let sha = self.commit(text);
            let seed = self.seed.to_string_lossy();
            sh(&self.origin, &["fetch", "--quiet", &seed, "+trunk:trunk"]);
            sha
        }

        fn files(&self) -> Files {
            Files::new(self.root.join("saved"))
        }

        fn path(&self) -> String {
            self.folder.to_string_lossy().into_owned()
        }
    }

    impl Drop for Repos {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn the_default_branch_is_read_from_the_servers_own_answer() {
        let sha = "a".repeat(40);
        assert_eq!(
            advertised_default(&format!("ref: refs/heads/trunk\tHEAD\n{sha}\tHEAD\n")),
            Some(("trunk".to_owned(), sha.clone()))
        );
        assert_eq!(
            advertised_default(&format!("ref: refs/heads/release/2.0\tHEAD\n{sha}\tHEAD\n")),
            Some(("release/2.0".to_owned(), sha.clone()))
        );
        // Only HEAD's target, only a branch, only a plain name, and only
        // with the commit it is at.
        assert_eq!(advertised_default(&format!("{sha}\tHEAD\n")), None);
        assert_eq!(advertised_default("ref: refs/heads/trunk\tHEAD\n"), None);
        assert_eq!(
            advertised_default(&format!("ref: refs/tags/v1\tHEAD\n{sha}\tHEAD\n")),
            None
        );
        assert_eq!(
            advertised_default(&format!("ref: refs/heads/main\tmain\n{sha}\tmain\n")),
            None
        );
        assert_eq!(
            advertised_default(&format!("ref: refs/heads/-x\tHEAD\n{sha}\tHEAD\n")),
            None
        );
        assert_eq!(
            advertised_default(&format!("ref: refs/heads/a..b\tHEAD\n{sha}\tHEAD\n")),
            None
        );
        assert_eq!(
            advertised_default("ref: refs/heads/trunk\tHEAD\nHEAD\tHEAD\n"),
            None
        );
        assert_eq!(advertised_default(""), None);
    }

    #[tokio::test]
    async fn no_login_in_the_folders_address_is_ever_in_an_argument() {
        let r = Repos::new();
        let made_up = "ghp_secret123";
        sh(
            &r.folder,
            &[
                "remote",
                "set-url",
                "origin",
                &format!("https://{}@github.com/acme/shop.git", login("me", made_up)),
            ],
        );
        let up = r.root.join("up.git");
        let own = folder_objects(&r.folder).await.expect("objects");
        for cmd in [
            ls_remote_command(&r.folder).await,
            fetch_command(&r.folder, &up, &own, "main")
                .await
                .expect("fetch"),
        ] {
            let args: Vec<String> = cmd
                .as_std()
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert!(
                args.iter()
                    .all(|a| !a.contains(made_up) && !a.contains("me:") && !a.contains("://")),
                "{args:?}"
            );
            assert!(args.contains(&"origin".to_owned()), "{args:?}");
        }
        let args = fetch_args("main");
        for want in [
            "--no-tags",
            "--refmap=",
            "--no-prune",
            "--no-write-fetch-head",
        ] {
            assert!(args.contains(&want.to_owned()), "{want} in {args:?}");
        }
        // One exact source and no destination: nothing is written in the
        // owner's folder.
        assert_eq!(args.last().map(String::as_str), Some("refs/heads/main"));
        // The saved default stays the same server's when a token changes.
        assert_eq!(
            credential_free(&format!(
                "https://{}@github.com/acme/shop.git",
                login("me", made_up)
            )),
            "https://me@github.com/acme/shop.git"
        );
        assert_eq!(
            credential_free("https://ghp_token@github.com/a/b"),
            "https://github.com/a/b"
        );
        assert_eq!(
            credential_free(&format!("https://{}@gitlab.com/a/b", "f".repeat(40))),
            "https://gitlab.com/a/b"
        );
        assert_eq!(
            credential_free(&format!("ssh://{}@host.example/a.git", login("git", "pw"))),
            "ssh://git@host.example/a.git"
        );
        assert_eq!(
            credential_free("git@github.com:acme/shop.git"),
            "git@github.com:acme/shop.git"
        );
    }

    /// The server's answer is used again for a minute, so requests that
    /// come together wait on the network once; after that it is asked again.
    #[tokio::test]
    async fn the_servers_answer_is_used_again_for_a_minute() {
        let r = Repos::new();
        let files = r.files();
        let b = sh(&r.origin, &["rev-parse", "trunk"]);
        assert_eq!(start_commit(&files, 1, &r.path()).await.expect("start"), b);
        let b2 = r.advance("b2");
        // Half the window: real time spent by the test itself (git is slow
        // on a busy Windows machine) stays well inside the rest.
        files.upstream.pass(Duration::from_secs(REUSE_SECS / 2));
        assert_eq!(start_commit(&files, 1, &r.path()).await.expect("start"), b);
        files.upstream.pass(Duration::from_secs(REUSE_SECS));
        assert_eq!(start_commit(&files, 1, &r.path()).await.expect("start"), b2);
    }

    /// How many packs and objects the app's server copy holds.
    fn held(files: &Files) -> (usize, String) {
        let up = files.upstream_cache(1);
        let packs = std::fs::read_dir(up.join("objects").join("pack"))
            .expect("packs")
            .filter(|e| {
                e.as_ref()
                    .is_ok_and(|e| e.path().extension().is_some_and(|x| x == "pack"))
            })
            .count();
        let counts = sh(&up, &["count-objects", "-v"])
            .lines()
            .filter(|l| l.starts_with("count:") || l.starts_with("in-pack:"))
            .collect::<Vec<_>>()
            .join(" ");
        (packs, counts)
    }

    /// The owner's folder sits well behind the server. Asking again at the
    /// same server commit, once the last answer is old, copies nothing more
    /// into the server copy: not the folder's missing history again, and
    /// not an empty pack.
    #[tokio::test]
    async fn asking_again_at_the_same_commit_adds_nothing() {
        let r = Repos::new();
        let mut tip = String::new();
        for n in 0..5 {
            tip = r.advance(&format!("b{n}"));
        }
        let files = r.files();
        assert_eq!(
            start_commit(&files, 1, &r.path()).await.expect("start"),
            tip
        );
        let first = held(&files);
        files.age_answers();
        assert_eq!(
            start_commit(&files, 1, &r.path()).await.expect("start"),
            tip
        );
        assert_eq!(held(&files), first);
    }

    #[test]
    fn what_a_fallback_logs_has_no_password_in_it() {
        let why = NotAsked::Failed(scrub(&format!(
            "fatal: unable to access 'https://{}@github.com/a/b.git/': 403",
            login("me", "ghp_secret123")
        )));
        let line = scrub(&why.to_string());
        assert!(
            !line.contains("ghp_secret123") && !line.contains("me:"),
            "{line}"
        );
        assert!(line.contains("https://github.com/a/b.git/"), "{line}");
    }

    /// The server answered, but its answer could not be taken into the
    /// hub's copy (here a stale lock on the ref that saves it): new work
    /// still gets a start, from the fallbacks, instead of an error.
    #[tokio::test]
    async fn an_answer_that_cannot_be_taken_in_falls_back() {
        let r = Repos::new();
        let files = r.files();
        let cache = files.app_cache(1);
        files.init_cache(&cache).await.expect("cache");
        let origin = sh(&r.folder, &["remote", "get-url", "origin"]);
        let id = identity(&r.path(), &credential_free(&origin));
        let refs = cache.join("refs").join("upstream-default");
        std::fs::create_dir_all(&refs).expect("refs dir");
        std::fs::write(refs.join(format!("{id}.lock")), "").expect("lock");
        let head = sh(&r.folder, &["rev-parse", "HEAD"]);
        assert_eq!(
            start_commit(&files, 1, &r.path()).await.expect("a start"),
            head
        );
    }

    #[test]
    fn a_branch_tip_is_read_only_for_that_exact_branch() {
        let sha = "b".repeat(40);
        assert_eq!(
            advertised_branch(&format!("{sha}\trefs/heads/main\n"), "main"),
            Some(sha.clone())
        );
        assert_eq!(
            advertised_branch(&format!("{sha}\trefs/heads/x/refs/heads/main\n"), "main"),
            None
        );
        assert_eq!(advertised_branch("HEAD\trefs/heads/main\n", "main"), None);
        assert_eq!(advertised_branch("", "main"), None);
    }

    /// Bring up to date gets the server's newest commit of the branch, pinned
    /// in the hub's copy, and never falls back to an older one.
    #[tokio::test]
    async fn the_tip_of_a_pull_requests_base_branch_comes_from_the_server_or_not_at_all() {
        let r = Repos::new();
        let files = r.files();
        let first = sh(&r.origin, &["rev-parse", "trunk"]);
        assert_eq!(
            branch_tip(&files, 1, &r.path(), "trunk")
                .await
                .expect("tip"),
            first
        );
        // Pinned for merging only: never a start a goal's helpers may ask
        // for with `?at=`.
        assert!(files.is_pinned_merge(1, &first).await);
        assert!(!files.is_pinned_start(1, &first).await);
        // The server moved on, the owner's folder did not: the server's wins.
        let next = r.advance("b2");
        let tip = branch_tip(&files, 1, &r.path(), "trunk")
            .await
            .expect("tip");
        assert_eq!(tip, next);
        assert!(files.is_pinned_merge(1, &tip).await);
        assert!(!files.is_pinned_start(1, &tip).await);
        assert_ne!(sh(&r.folder, &["rev-parse", "HEAD"]), next);
        // No such branch, an odd name, or no server: refused in plain words.
        for b in ["nope", "-x"] {
            match branch_tip(&files, 1, &r.path(), b).await {
                Err(StoreError::Refused(m)) => assert!(m.contains("could not get"), "{m}"),
                other => panic!("{b}: {other:?}"),
            }
        }
        let gone = r.root.join("gone.git").to_string_lossy().into_owned();
        sh(&r.folder, &["remote", "set-url", "origin", &gone]);
        assert!(matches!(
            branch_tip(&files, 1, &r.path(), "trunk").await,
            Err(StoreError::Refused(_))
        ));
        sh(&r.folder, &["remote", "remove", "origin"]);
        assert!(matches!(
            branch_tip(&files, 1, &r.path(), "trunk").await,
            Err(StoreError::Refused(_))
        ));
    }

    #[test]
    fn a_fallback_is_told_once_per_folder_until_the_server_answers_again() {
        let locks = Locks::default();
        assert!(locks.first_fallback("aaaa"));
        assert!(!locks.first_fallback("aaaa"));
        assert!(locks.first_fallback("bbbb"));
        locks.answered("aaaa");
        assert!(locks.first_fallback("aaaa"));
    }

    #[test]
    fn each_folder_and_address_has_its_own_saved_default() {
        let a = identity("C:\\repos\\shop", "https://github.com/a/shop.git");
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(
            a,
            identity("C:\\repos\\shop", "https://github.com/a/shop.git")
        );
        assert_ne!(
            a,
            identity("C:\\repos\\other", "https://github.com/a/shop.git")
        );
        assert_ne!(
            a,
            identity("C:\\repos\\shop", "https://github.com/a/fork.git")
        );
    }
}
