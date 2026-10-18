use anyhow::{bail, Context};
use decurion_proto::browser::{FromBrowser, SiteState};
use decurion_proto::context::ContextRef;
use decurion_proto::goal::{
    HelperClaim, HelperResult, HelperSource, ResearchReport, MAX_PAGE_BYTES,
};
use decurion_proto::{
    sites, App, Claim, HelloReply, JoinReply, JoinRequest, Live, MoveRequest, NewCheckpoint, Place,
    RunCredential, RunEvent, RunnerHello, SignInSession, ThreadState, RULES_VERSION,
};
use decurion_runner::dispatch;
use decurion_runner::gatekeeper::{Gatekeeper, Grant, ANTHROPIC_API};
use decurion_runner::handover::{
    bundle_spec, context_import_args, host_git_args, host_git_env, is_sha, move_note,
    reuse_workspace, save_edits_args, session_export_args, session_import_args,
    tool_container_name, CODE_CAP, CONTEXT_CAP, SESSION_CAP,
};
use decurion_runner::identity::{
    hub_url, place_from_env, write_atomic, CloudRunner, PC_HUB, PC_NAME,
};
use decurion_runner::sign_ins::{self, HubAccess};
mod addon_box;
mod browser_box;
mod preview_box;

use decurion_runner::{
    backoff, browser_container_name, check_home_volume, clear_report, container_name,
    context_prompt, design_home_volume, goal_home_volume, helper_container_name, helper_run_args,
    helper_tool_container_name, hide, hide_cut, home_volume, is_interrupted_turn, is_turn_start,
    network_name, parse_line_hiding, parse_result, part_done, patient_data_prompt, pi_run_args,
    prompt_for, read_page, read_report, redact, research_outcome, retriable, run_args, sibling,
    valid_branch, Acks, Action, ContextMeter, Conversation, Credential, FirstResult, LeakScanner,
    PiRun, Sandbox, Start, Step, BUDGET_STOP, DOCKER_CLI_ENV, LIMIT_STOP,
};
use decurion_runner::{gate_args, parse_gate_line, BoxNet, GateLine, GATE_IMAGE, RELAY_URL};
use decurion_vault::{check_name, Vault};
use reqwest::header::{HeaderMap, CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::StatusCode;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// How long the last steps of a run (saving a checkpoint, reporting the
/// outcome) keep trying while the hub is out of reach. A cloud worker's hub is
/// the owner's PC, which may be asleep or off for days; giving up would leave
/// the work shown as running forever, so only a refusal ends these early.
const FINAL_RETRY: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// How long a download keeps trying. The hub just handed out the work, so it
/// is awake; this only covers a short hiccup.
const DOWNLOAD_RETRY: Duration = Duration::from_secs(2 * 60);
/// Longest a throwaway container may take.
const TOOL_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// A hub call that did not work: the hub could not be reached, or it said no.
#[derive(Debug)]
enum HubError {
    Unreachable(String, reqwest::Error),
    Refused(String, StatusCode, String),
}

impl std::fmt::Display for HubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HubError::Unreachable(path, e) => write!(f, "hub {path}: unreachable: {e}"),
            HubError::Refused(path, s, text) => write!(f, "hub {path}: {s} {text}"),
        }
    }
}

impl std::error::Error for HubError {}

impl HubError {
    fn retriable(&self) -> bool {
        retriable(match self {
            HubError::Unreachable(..) => None,
            HubError::Refused(_, s, _) => Some(s.as_u16()),
        })
    }
}

/// How often a run asks the hub for owner messages and whether it was stopped.
const POLL: Duration = Duration::from_secs(2);
/// How long Claude may print nothing at all after a result while stdin is
/// still open before the runner decides the turn it waits for is not coming.
const IDLE_LIMIT: Duration = Duration::from_secs(5 * 60);
/// Most ids the hub marks delivered in one call.
const ACK_BATCH: usize = 500;

/// The status the hub answered with, if the error is the hub saying no.
fn hub_status(err: &HubError) -> Option<StatusCode> {
    match err {
        HubError::Refused(_, s, _) => Some(*s),
        HubError::Unreachable(..) => None,
    }
}

/// Cheap to clone: the work loop and the helper loop each hold one.
#[derive(Clone)]
struct Hub {
    base: String,
    key: String,
    http: reqwest::Client,
    /// This runner's name, bound by the hub to its key.
    name: String,
    /// The computer this runner is; it only runs work placed here.
    place: Place,
}

impl Hub {
    /// What the sign-in calls need from this connection.
    fn access(&self) -> HubAccess<'_> {
        HubAccess {
            http: &self.http,
            base: &self.base,
            key: &self.key,
        }
    }
}

/// Short calls to the hub give up after this, so a hub that went to sleep
/// mid-call does not hang the runner.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// What the hub answers, with a 500, when its database failed and nothing
/// was saved (`ApiError` in the hub's `lib.rs`).
const HUB_DB_ERROR: &str = "database error";

impl Hub {
    fn url(&self, path: &str) -> String {
        format!("{}/api{path}", self.base)
    }

    async fn send(
        &self,
        path: &str,
        req: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, HubError> {
        let r = req
            .bearer_auth(&self.key)
            .send()
            .await
            .map_err(|e| HubError::Unreachable(path.to_owned(), e))?;
        if !r.status().is_success() {
            let s = r.status();
            return Err(HubError::Refused(
                path.to_owned(),
                s,
                r.text().await.unwrap_or_default(),
            ));
        }
        Ok(r)
    }

    /// What a run checks while it goes. Short timeout: a slow hub must not
    /// hold up reading the agent's output for long.
    async fn live(&self, thread: i64) -> anyhow::Result<Live> {
        let path = format!("/threads/{thread}/live");
        let req = self
            .http
            .get(self.url(&path))
            .timeout(Duration::from_secs(10));
        Ok(self.send(&path, req).await?.json().await?)
    }

    async fn delivered(&self, thread: i64, ids: &[i64]) -> anyhow::Result<()> {
        self.post(
            &format!("/threads/{thread}/messages/delivered"),
            &serde_json::json!({ "ids": ids }),
        )
        .await?;
        Ok(())
    }

    async fn post<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<reqwest::Response, HubError> {
        let req = self
            .http
            .post(self.url(path))
            .timeout(CALL_TIMEOUT)
            .json(body);
        self.send(path, req).await
    }

    /// Posts one line of the work's activity. When the hub itself answers
    /// that its database failed, it rolled the line back and saved nothing,
    /// so the line is sent once more rather than lost. Nothing else is
    /// retried: a proxy's error or a lost answer may come after the hub saved
    /// it, and a second copy would show twice. One short retry only, so a
    /// struggling hub cannot hold up reading the agent's output for long.
    async fn event(&self, thread: i64, e: &RunEvent) {
        let path = format!("/threads/{thread}/events");
        let mut r = self.post(&path, e).await;
        if matches!(&r, Err(HubError::Refused(_, s, body))
            if *s == StatusCode::INTERNAL_SERVER_ERROR && body == HUB_DB_ERROR)
        {
            tokio::time::sleep(Duration::from_millis(500)).await;
            r = self.post(&path, e).await;
        }
        if let Err(err) = r {
            tracing::warn!(%err, thread, "could not post event");
        }
    }

    async fn move_to(&self, thread: i64, to: ThreadState, note: &str) -> anyhow::Result<()> {
        self.post(
            &format!("/threads/{thread}/move"),
            &MoveRequest {
                to,
                note: Some(note.to_owned()),
                expect: None,
                receipt: None,
            },
        )
        .await?;
        Ok(())
    }

    /// Saves a download to `dest` as it arrives, never holding it in memory,
    /// and stops at `cap` bytes. Returns the response headers.
    async fn download(&self, path: &str, dest: &Path, cap: u64) -> anyhow::Result<HeaderMap> {
        let mut r = self.send(path, self.http.get(self.url(path))).await?;
        if r.content_length().is_some_and(|n| n > cap) {
            bail!("hub {path}: larger than the {cap}-byte limit");
        }
        let headers = r.headers().clone();
        let mut f = tokio::fs::File::create(dest).await?;
        let mut n: u64 = 0;
        loop {
            let chunk = tokio::time::timeout(CALL_TIMEOUT, r.chunk())
                .await
                .with_context(|| format!("hub {path}: download stalled"))?
                .map_err(|e| HubError::Unreachable(path.to_owned(), e))?;
            let Some(chunk) = chunk else { break };
            n += chunk.len() as u64;
            if n > cap {
                bail!("hub {path}: larger than the {cap}-byte limit");
            }
            f.write_all(&chunk).await?;
        }
        f.flush().await?;
        Ok(headers)
    }

    /// Uploads a file as the raw request body, streamed from disk.
    async fn put_file(&self, path: &str, file: &Path) -> anyhow::Result<()> {
        let f = tokio::fs::File::open(file).await?;
        let len = f.metadata().await?.len();
        let req = self
            .http
            .put(self.url(path))
            .header(CONTENT_TYPE, "application/octet-stream")
            .header(CONTENT_LENGTH, len)
            .body(reqwest::Body::from(f));
        self.send(path, req).await?;
        Ok(())
    }
}

/// Keeps trying `f` while the hub is out of reach or briefly failing, for up
/// to `limit`, waiting longer each time. A refusal is returned at once.
async fn with_retry<T>(
    what: &str,
    limit: Duration,
    mut f: impl AsyncFnMut() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let start = Instant::now();
    let mut attempt = 0;
    loop {
        let err = match f().await {
            Ok(v) => return Ok(v),
            Err(e) => e,
        };
        let wait = backoff(attempt);
        let again = err
            .downcast_ref::<HubError>()
            .is_some_and(HubError::retriable);
        if !again || start.elapsed() + wait > limit {
            return Err(err);
        }
        tracing::warn!(
            %err,
            wait_secs = wait.as_secs(),
            "could not {what} yet; waiting for the hub to answer, then trying again"
        );
        tokio::time::sleep(wait).await;
        attempt += 1;
    }
}

fn data_dir() -> anyhow::Result<PathBuf> {
    if let Ok(d) = std::env::var("DECURION_DATA") {
        return Ok(PathBuf::from(d));
    }
    let base = std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("HOME"))
        .context("set DECURION_DATA")?;
    Ok(PathBuf::from(base).join("decurion"))
}

/// The run's one credential, from Windows Credential Manager: a Max profile's
/// token (`claude setup-token`, saved as `decurion/claude-profile-A`), the
/// pay-per-use API key (`decurion/anthropic-api`), which stays with the
/// gatekeeper, or the OpenAI or xAI key (`decurion/openai-api`,
/// `decurion/xai-api`).
fn secret(cred: Credential) -> anyhow::Result<String> {
    // A Max plan's sign-in saved from the board ("Sign in on this PC") is in
    // the vault: the same Credential Manager entry on Windows, Decurion's own
    // private folder on Mac and Linux.
    if let Credential::Max(p) = cred {
        if let Some(t) = Vault::system(&data_dir()?).get_claude(p)? {
            return Ok(t);
        }
    }
    let target = cred.target();
    #[cfg(windows)]
    {
        let entry = keyring::Entry::new_with_target(&target, "decurion", "claude")?;
        entry
            .get_password()
            .map(|s| s.trim().to_owned())
            .with_context(|| format!("nothing saved in Credential Manager as {target}"))
    }
    #[cfg(not(windows))]
    {
        let var = cred.env_fallback();
        std::env::var(&var)
            .map(|s| s.trim().to_owned())
            .with_context(|| format!("{var} not set ({target})"))
    }
}

/// The app's test keys as (name, value), from this PC's key store. The hub
/// sends only their names.
fn test_keys(hub: &Hub, data: &Path, c: &Claim) -> anyhow::Result<Vec<(String, String)>> {
    vault_keys(hub, data, c.app.id, &c.test_keys)
}

/// Test keys `names` of app `app_id` as (name, value), from this PC's key
/// store.
fn vault_keys(
    hub: &Hub,
    data: &Path,
    app_id: i64,
    names: &[String],
) -> anyhow::Result<Vec<(String, String)>> {
    // Test keys live only in the PC's key store, which never leaves it.
    if hub.place == Place::Cloud && !names.is_empty() {
        bail!(
            "this app uses test keys, which stay on the PC, so its work cannot run on the \
cloud worker yet. Move it to the PC and try again."
        );
    }
    let vault = Vault::system(data);
    names
        .iter()
        .map(|name| {
            // The hub checked the name when it was saved; check it again here,
            // where it becomes an environment variable.
            check_name(name).map_err(|e| anyhow::anyhow!("test key {name:?}: {e}"))?;
            let value = vault
                .get(app_id, name)
                .map_err(|e| anyhow::anyhow!("test key {name}: {e}"))?
                .with_context(|| {
                    format!(
                        "test key {name} is listed on the board but is missing from this PC's \
key store; save it again on the board"
                    )
                })?;
            Ok((name.clone(), value))
        })
        .collect()
}

/// The run's credential. The PC keeps its own; a cloud worker keeps none and
/// is handed the one this run needs by the hub.
async fn credential(hub: &Hub, id: i64, cred: Credential) -> anyhow::Result<String> {
    if hub.place == Place::Pc {
        return secret(cred);
    }
    let r: RunCredential = hub
        .post(
            &format!("/runner/threads/{id}/credential"),
            &serde_json::json!({ "runner": hub.name }),
        )
        .await?
        .json()
        .await
        .context("could not read the run's credential")?;
    // Only the one variable this run should get, never anything else.
    let want = cred.vars().first().copied().unwrap_or_default();
    if r.var != want {
        bail!("hub sent {} but this run needs {want}", r.var);
    }
    Ok(r.value)
}

/// Runs git on the host. It never trusts any config but the runner's own (see
/// [`host_git_env`]), so a `.gitattributes` or `.lfsconfig` the agent
/// committed cannot start filters or Git LFS here.
async fn git(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git")
        .envs(host_git_env())
        .args(host_git_args())
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .await?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Like [`git`], but ignoring the PC's system and user git settings, for
/// commands that handle files an agent wrote. Long paths are on for the
/// same reason as in [`host_git_args`].
async fn git_isolated(dir: &Path, args: &[&str], null: &str) -> anyhow::Result<String> {
    let out = Command::new("git")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null)
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .args(["-c", "core.longpaths=true"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .await?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Removes a folder or file, if there is one.
fn remove_all(p: &Path) -> std::io::Result<()> {
    let r = if p.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    };
    match r {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// An empty folder for one thread's downloads and uploads.
fn transfer_dir(data: &Path, id: i64) -> anyhow::Result<PathBuf> {
    let d = data.join("transfer").join(id.to_string());
    remove_all(&d)?;
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

/// The thread's own copy of the code, on its own branch, and whether it had
/// to be rebuilt from the hub.
///
/// The runner's copy is reused only when it is the one the hub's latest
/// checkpoint was made from. Otherwise (new work, or work saved on another
/// computer) it is rebuilt from what the hub holds: the starting commit, plus
/// the latest checkpoint's branch and the agent's session. Code comes only
/// from the hub, on the PC too.
///
/// A full clone, not a `git worktree`: a worktree's `.git` file points at a
/// folder on the host, which does not exist inside the container.
///
/// Once an agent has run in this folder it controls its git config and hooks,
/// so the runner never runs git in it again (see [`new_commits`]); git runs
/// here only in the fresh clone the runner just made.
async fn workspace(
    hub: &Hub,
    data: &Path,
    c: &Claim,
    branch: &str,
) -> anyhow::Result<(PathBuf, bool)> {
    let id = c.thread.id;
    let dir = data
        .join("workspaces")
        .join(format!("{}-{}", c.app.name, id));
    let marker_file = sibling(&dir, "checkpoint");
    let marker = match std::fs::read_to_string(&marker_file) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        // Unreadable: matches no checkpoint, so the copy is rebuilt.
        Err(_) => Some(String::new()),
    };
    if reuse_workspace(dir.join(".git").is_dir(), marker.as_deref(), c.checkpoint) {
        // Read the file instead of asking git, which would run the agent's config.
        let head = std::fs::read_to_string(dir.join(".git").join("HEAD")).unwrap_or_default();
        if head.trim() != format!("ref: refs/heads/{branch}") {
            bail!("workspace {} is not on branch {branch}", dir.display());
        }
        return Ok((dir, false));
    }
    tracing::info!(id, checkpoint = ?c.checkpoint, "rebuilding this work's copy from the hub");
    // A work folder can be large; deleting it must not stall the other
    // pieces running alongside (their stop and budget checks share this task).
    let aside = dir.clone();
    tokio::task::spawn_blocking(move || set_aside(&aside))
        .await
        .context("setting the old copy aside stopped")??;
    std::fs::create_dir_all(dir.parent().context("workspace parent")?)?;
    let tmp = sibling(&dir, "partial");
    let old = tmp.clone();
    tokio::task::spawn_blocking(move || remove_all(&old))
        .await
        .context("clearing an unfinished copy stopped")??;
    let xfer = transfer_dir(data, id)?;

    let base_bundle = xfer.join("base.bundle");
    let headers = with_retry("download the starting commit", DOWNLOAD_RETRY, async || {
        hub.download(
            &format!("/runner/threads/{id}/base"),
            &base_bundle,
            CODE_CAP,
        )
        .await
    })
    .await?;
    let base = headers
        .get("x-decurion-base")
        .and_then(|v| v.to_str().ok())
        .filter(|s| is_sha(s))
        .context("hub sent no starting commit")?
        .to_owned();
    let tmp_s = tmp.to_string_lossy().into_owned();
    let bundle_s = base_bundle.to_string_lossy().into_owned();
    // `--no-hardlinks` matters when cloning from a folder: a hardlinked
    // object file would be shared with the source, and an agent could
    // overwrite it in place.
    git(
        Path::new("."),
        &[
            "clone",
            "--quiet",
            "--no-checkout",
            "--no-hardlinks",
            "--",
            &bundle_s,
            &tmp_s,
        ],
    )
    .await?;
    // A Bring up to date run gets main from the same bundle.
    if let Some(m) = c.merge_from.as_deref() {
        prepare_main(&tmp, m).await?;
    }
    // The bundle file is about to be deleted; leave no remote pointing at it.
    git(&tmp, &["remote", "remove", "origin"]).await?;
    match c.checkpoint {
        Some(cp) => {
            let code = xfer.join("code.bundle");
            with_retry("download the saved code", DOWNLOAD_RETRY, async || {
                hub.download(
                    &format!("/runner/threads/{id}/checkpoint/{cp}/code"),
                    &code,
                    CODE_CAP,
                )
                .await
            })
            .await?;
            let code_s = code.to_string_lossy().into_owned();
            let refspec = format!("+refs/heads/{branch}:refs/heads/{branch}");
            git(
                &tmp,
                &["fetch", "--quiet", "--no-tags", "--", &code_s, &refspec],
            )
            .await?;
            git(&tmp, &["switch", "--quiet", branch]).await?;
        }
        None => {
            git(&tmp, &["switch", "--quiet", "-c", branch, &base]).await?;
        }
    }

    // The session goes with the code: a fresh home, plus the saved sessions.
    let out = docker()
        .args(["volume", "rm", "-f", &home_volume(id)])
        .output()
        .await?;
    if !out.status.success() {
        bail!(
            "could not reset the agent's home: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    if let Some(cp) = c.checkpoint {
        let tar = xfer.join("session.tar");
        with_retry("download the saved session", DOWNLOAD_RETRY, async || {
            hub.download(
                &format!("/runner/threads/{id}/checkpoint/{cp}/session"),
                &tar,
                SESSION_CAP,
            )
            .await
        })
        .await?;
        import_session(id, &tar).await?;
    }

    // The starting commit and the checkpoint marker live next to the
    // workspace, outside the container's mount, so the agent cannot move them.
    write_atomic(&base_file(&dir), base.as_bytes(), false)?;
    if let Some(cp) = c.checkpoint {
        write_atomic(&marker_file, cp.to_string().as_bytes(), false)?;
    }
    // Only a fully prepared copy gets the real name, so a crash mid-setup
    // never leaves a half-made workspace that looks finished.
    std::fs::rename(&tmp, &dir)?;
    let _ = remove_all(&xfer);
    Ok((dir, true))
}

/// Makes the local branch `decurion-main` at exactly `merge_from`, in a copy
/// just cloned from the hub's starting bundle, for a Bring up to date run.
/// The bundle must have named that commit as `refs/heads/decurion-main`;
/// anything else stops the run before the agent starts.
async fn prepare_main(copy: &Path, merge_from: &str) -> anyhow::Result<()> {
    let given = git(
        copy,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "refs/remotes/origin/decurion-main^{commit}",
        ],
    )
    .await
    .ok();
    if !is_sha(merge_from) || given.as_deref() != Some(merge_from) {
        bail!(
            "The starting copy from the hub did not hold the newest main this work is to merge, \
so it did not start. Tap Bring up to date again on the pull request."
        );
    }
    git(copy, &["branch", "--no-track", "decurion-main", merge_from]).await?;
    Ok(())
}

/// Moves an out-of-date workspace (and its checkpoint and starting-commit
/// files) to `<workspace>.previous` instead of deleting it: it may hold work
/// that never reached the hub. Only the latest such copy is kept.
fn set_aside(dir: &Path) -> anyhow::Result<()> {
    let prev = sibling(dir, "previous");
    let pairs = [
        (dir.to_path_buf(), prev.clone()),
        (sibling(dir, "checkpoint"), sibling(&prev, "checkpoint")),
        (base_file(dir), base_file(&prev)),
    ];
    if !dir.exists() {
        // Nothing to keep; only stray files from an older copy.
        for (from, _) in &pairs {
            remove_all(from)?;
        }
        return Ok(());
    }
    for (_, to) in &pairs {
        remove_all(to)?;
    }
    for (from, to) in &pairs {
        match std::fs::rename(from, to) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(e).with_context(|| format!("could not set aside {}", from.display()))
            }
            _ => {}
        }
    }
    tracing::info!(
        kept = %prev.display(),
        "this computer's copy of the work was out of date; kept it aside in case it holds unsaved work"
    );
    Ok(())
}

/// `git clone --no-hardlinks` of the app's repo. See [`workspace`] for why
/// hardlinks are off.
async fn clone_into(from: &str, to: &Path) -> anyhow::Result<()> {
    let to_s = to.to_string_lossy().into_owned();
    git(
        Path::new("."),
        &["clone", "--quiet", "--no-hardlinks", "--", from, &to_s],
    )
    .await
    .map(|_| ())
}

/// Where the starting commit is kept: next to the workspace, outside the
/// container's mount, so the agent cannot move it.
fn base_file(dir: &Path) -> PathBuf {
    sibling(dir, "base")
}

/// Fetches the agent's branch into a runner-owned bare repo with hooks off.
/// Nothing the agent wrote is trusted except the commits themselves.
async fn fetch_branch(data: &Path, dir: &Path, branch: &str) -> anyhow::Result<PathBuf> {
    let bare = review_repo(data, dir)?;
    if !bare.exists() {
        std::fs::create_dir_all(&bare)?;
        git(&bare, &["init", "--quiet", "--bare"]).await?;
    }
    let src = dir.to_string_lossy().into_owned();
    let refspec = format!("+refs/heads/{branch}:refs/heads/{branch}");
    // The source repo's config is agent-written. Command-line settings win
    // over it and reach git's child processes: on top of what [`git`] always
    // turns off (hooks, fsmonitor, lazy fetching), no transports beyond plain
    // local files.
    git(
        &bare,
        &[
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.file.allow=always",
            "fetch",
            "--quiet",
            "--no-tags",
            "--",
            &src,
            &refspec,
        ],
    )
    .await?;
    Ok(bare)
}

/// The recorded starting commit of a workspace.
fn read_base(dir: &Path) -> anyhow::Result<String> {
    let base = std::fs::read_to_string(base_file(dir)).context("missing base commit")?;
    let base = base.trim();
    if !is_sha(base) {
        bail!("base commit file holds no commit id");
    }
    Ok(base.to_owned())
}

/// Commits the agent added on its branch, counted in the runner-owned bare
/// repo against the recorded starting commit and, for a Bring up to date
/// run, the main commit it merged in ([`new_revs`]). A merge commit counts.
async fn new_commits(
    data: &Path,
    dir: &Path,
    branch: &str,
    merge_from: Option<&str>,
) -> anyhow::Result<u64> {
    let revs = new_revs(dir, branch, merge_from)?;
    let bare = fetch_branch(data, dir, branch).await?;
    let mut args = vec!["rev-list", "--count"];
    args.extend(revs.iter().map(String::as_str));
    args.push("--");
    Ok(git(&bare, &args).await?.parse()?)
}

/// The agent's own work on its branch, as `git rev-list` arguments: the
/// branch, without anything the recorded start reaches and, for a Bring up
/// to date run, anything `merge_from` reaches. Main's own history came with
/// the merge; it is not the agent's to count or scan.
fn new_revs(dir: &Path, branch: &str, merge_from: Option<&str>) -> anyhow::Result<Vec<String>> {
    let base = read_base(dir)?;
    let mut revs = vec![format!("refs/heads/{branch}"), "--not".to_owned(), base];
    if let Some(m) = merge_from {
        if !is_sha(m) {
            bail!("the commit to merge is not a commit id");
        }
        revs.push(m.to_owned());
    }
    Ok(revs)
}

/// Whether a Bring up to date run merged `merge_from` the way it was
/// asked: its branch descends from both the start and `merge_from`, and
/// its new commits pass [`decurion_proto::merged_as_asked`]. Read in the
/// runner-owned bare repo; anything git cannot answer counts as no.
async fn merged_main(
    data: &Path,
    dir: &Path,
    branch: &str,
    merge_from: &str,
) -> anyhow::Result<bool> {
    let base = read_base(dir)?;
    if !is_sha(merge_from) {
        bail!("the commit to merge is not a commit id");
    }
    let bare = fetch_branch(data, dir, branch).await?;
    let head = format!("refs/heads/{branch}");
    for from in [base.as_str(), merge_from] {
        if git(&bare, &["merge-base", "--is-ancestor", from, &head])
            .await
            .is_err()
        {
            return Ok(false);
        }
    }
    let list = git(
        &bare,
        &[
            "rev-list",
            "--parents",
            &head,
            "--not",
            &base,
            merge_from,
            "--",
        ],
    )
    .await?;
    Ok(decurion_proto::merged_as_asked(&list, merge_from))
}

async fn count_since(bare: &Path, base: &str, branch: &str) -> anyhow::Result<u64> {
    let range = format!("{base}..refs/heads/{branch}");
    let n = git(bare, &["rev-list", "--count", &range]).await?;
    Ok(n.parse()?)
}

/// Runs a throwaway container to the end, within [`TOOL_TIMEOUT`]. `name` is
/// the container's name in `args`, removed if it takes too long.
async fn run_tool(
    name: &str,
    args: &[String],
    stdin: Stdio,
) -> anyhow::Result<std::process::Output> {
    let child = docker()
        .args(args)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("could not start docker")?;
    match tokio::time::timeout(TOOL_TIMEOUT, child.wait_with_output()).await {
        Ok(out) => Ok(out?),
        Err(_) => {
            let _ = docker().args(["rm", "-f", name]).output().await;
            bail!("throwaway container took too long")
        }
    }
}

/// Saves the agent's unfinished edits as a commit, inside a sealed throwaway
/// container, so they travel with the work.
async fn save_edits(id: i64, wt: &Path) -> anyhow::Result<()> {
    let [add, commit] = save_edits_args(id, wt)?;
    let tool = tool_container_name(id);
    let out = run_tool(&tool, &add, Stdio::null()).await?;
    if !out.status.success() {
        bail!(
            "git add in the agent's copy failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let out = run_tool(&tool, &commit, Stdio::null()).await?;
    if !out.status.success() {
        // Usually: nothing was left to commit.
        tracing::info!(
            id,
            out = %String::from_utf8_lossy(&out.stdout).trim(),
            "no unfinished edits saved"
        );
    }
    Ok(())
}

/// Unpacks a saved session into the agent's home volume, inside a sealed
/// throwaway container.
async fn import_session(id: i64, tar: &Path) -> anyhow::Result<()> {
    let f = std::fs::File::open(tar)?;
    let out = run_tool(
        &tool_container_name(id),
        &session_import_args(id),
        Stdio::from(f),
    )
    .await?;
    if !out.status.success() {
        bail!(
            "could not restore the agent's session: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Writes the agent's sessions as a tar archive to `dest`, from a sealed
/// throwaway container, stopping at [`SESSION_CAP`].
async fn export_session(id: i64, dest: &Path) -> anyhow::Result<()> {
    let mut child = docker()
        .args(session_export_args(id))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("could not start docker")?;
    let mut out = child.stdout.take().context("no stdout")?;
    let copy = async {
        let mut f = tokio::fs::File::create(dest).await?;
        let mut buf = vec![0u8; 64 << 10];
        let mut n: u64 = 0;
        loop {
            let k = out.read(&mut buf).await?;
            if k == 0 {
                break;
            }
            n += k as u64;
            if n > SESSION_CAP {
                bail!("the agent's session is larger than {SESSION_CAP} bytes");
            }
            f.write_all(&buf[..k]).await?;
        }
        f.flush().await?;
        anyhow::Ok(())
    };
    let copied = match tokio::time::timeout(TOOL_TIMEOUT, copy).await {
        Ok(r) => r,
        Err(_) => Err(anyhow::anyhow!("saving the agent's session took too long")),
    };
    if let Err(e) = copied {
        let _ = docker()
            .args(["rm", "-f", &tool_container_name(id)])
            .output()
            .await;
        return Err(e);
    }
    let status = child.wait().await?;
    if !status.success() {
        bail!("tar of the agent's session exited with {status}");
    }
    Ok(())
}

/// An empty tar archive: two zero blocks, padded to a full record.
const EMPTY_TAR: [u8; 10240] = [0; 10240];

/// Puts the owner's attached files in a fresh `/home/agent/context` in
/// `home_volume`, before the agent's box starts: downloaded from the hub's
/// `download` path into `xfer` (the job's own empty transfer folder) and
/// unpacked in a sealed throwaway box named `tool`, which also adds PDF text
/// versions. With no files nothing is downloaded, but the folder is still
/// replaced (from an empty archive), so a file the owner removed is gone
/// from the box too. The download is deleted afterwards.
///
/// The error is for the owner: the run does not start without the files the
/// owner gave it, so a failure here is shown rather than skipped.
async fn put_context(
    hub: &Hub,
    files: &[ContextRef],
    download: &str,
    xfer: &Path,
    tool: &str,
    home_volume: &str,
) -> anyhow::Result<()> {
    let tar = xfer.join("context.tar");
    let r = async {
        if files.is_empty() {
            std::fs::write(&tar, EMPTY_TAR)?;
        } else {
            // Not `with_retry`: helper jobs run this too, on a task its
            // async closures cannot be sent to.
            download_retrying(hub, download, &tar, CONTEXT_CAP).await?;
        }
        // A box left by a runner that died.
        let _ = docker_out(&["rm", "-f", tool]).await;
        let f = std::fs::File::open(&tar)?;
        let out = run_tool(
            tool,
            &context_import_args(tool, home_volume),
            Stdio::from(f),
        )
        .await?;
        if !out.status.success() {
            bail!(
                "unpacking box exited with {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        anyhow::Ok(())
    }
    .await;
    let _ = remove_all(&tar);
    if let Err(err) = r {
        tracing::warn!(err = %format!("{err:#}"), %home_volume, "could not put the attached files in place");
        bail!(
            "Could not give the agent the files attached to this app, so the work did not start. \
Try again in a moment."
        );
    }
    tracing::info!(files = files.len(), %home_volume, "attached files in place");
    Ok(())
}

/// Saves the work to the hub so any computer can continue it: the branch as a
/// git bundle and the agent's sessions as a tar archive. Records the
/// checkpoint next to the workspace, so the next run here knows its own copy
/// is the latest. Keeps trying while the hub is out of reach.
async fn save_checkpoint(
    hub: &Hub,
    data: &Path,
    id: i64,
    wt: &Path,
    branch: &str,
) -> anyhow::Result<i64> {
    let base = read_base(wt)?;
    let bare = fetch_branch(data, wt, branch).await?;
    let head = git(
        &bare,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch}^{{commit}}"),
        ],
    )
    .await?;
    // A branch rewritten past its starting commit cannot be counted; send it
    // whole.
    let n = count_since(&bare, &base, branch).await.unwrap_or(0);
    let xfer = transfer_dir(data, id)?;
    let code = xfer.join("code.bundle");
    let code_s = code.to_string_lossy().into_owned();
    git(
        &bare,
        &[
            "bundle",
            "create",
            "--quiet",
            &code_s,
            &bundle_spec(&base, branch, n),
        ],
    )
    .await?;
    let tar = xfer.join("session.tar");
    if let Err(err) = export_session(id, &tar).await {
        // The code matters most; without a session the agent starts afresh
        // on it, which is better than losing the code.
        tracing::warn!(%err, id, "could not save the agent's session; saving the code only");
        std::fs::write(&tar, EMPTY_TAR)?;
    }
    let cp = with_retry("save a checkpoint", FINAL_RETRY, async || {
        hub.put_file(&format!("/runner/threads/{id}/upload/code"), &code)
            .await?;
        hub.put_file(&format!("/runner/threads/{id}/upload/session"), &tar)
            .await?;
        let v: serde_json::Value = hub
            .post(
                &format!("/runner/threads/{id}/checkpoint"),
                &NewCheckpoint {
                    head: head.clone(),
                    base: Some(base.clone()),
                },
            )
            .await?
            .json()
            .await?;
        v["id"].as_i64().context("hub gave no checkpoint id")
    })
    .await?;
    write_atomic(&sibling(wt, "checkpoint"), cp.to_string().as_bytes(), false)?;
    let _ = remove_all(&xfer);
    tracing::info!(id, checkpoint = cp, "saved a checkpoint");
    Ok(cp)
}

/// The runner-owned bare repo the agent's commits are fetched into.
fn review_repo(data: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
    let name = dir.file_name().context("workspace name")?;
    Ok(sibling(&data.join("review").join(name), "git"))
}

fn no_hooks() -> String {
    format!("core.hooksPath={}", null_device())
}

/// The empty file git settings can point at to turn something off.
fn null_device() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

/// Names of the test keys whose values appear anywhere in the agent's new
/// commits, read from the review repo after [`new_commits`] fetched them.
/// Every object the branch adds (commits, trees and file contents) is read
/// raw, exactly as it would be pushed, so a value cannot hide in a binary
/// file, a file name, an author field or a commit message, nor behind
/// anything git would leave out when printing a commit.
///
/// A key whose value was already in the code the agent started from is not
/// counted: the app's repo holds it anyway (publishable keys often are), and
/// any change to the file it sits in would otherwise stop the work.
///
/// For a Bring up to date run, `merge_from` is left out too ([`new_revs`]):
/// what main already held is not the agent's, while a file the merge
/// made new (a resolved conflict) is scanned like any other.
async fn leaked_keys(
    data: &Path,
    dir: &Path,
    branch: &str,
    merge_from: Option<&str>,
    keys: &[(String, String)],
) -> anyhow::Result<Vec<String>> {
    if keys.is_empty() {
        return Ok(vec![]);
    }
    let bare = review_repo(data, dir)?;
    let revs = new_revs(dir, branch, merge_from)?;
    let mut args = vec!["rev-list", "--objects", "--no-object-names"];
    args.extend(revs.iter().map(String::as_str));
    args.push("--");
    let objects = git(&bare, &args).await?;
    let found = scan_objects(&bare, &objects, keys).await?;
    if found.is_empty() {
        return Ok(found);
    }
    let base = std::fs::read_to_string(base_file(dir)).context("missing base commit")?;
    let listing = git(&bare, &["ls-tree", "-r", "-z", base.trim(), "--"]).await?;
    // Each entry is "<mode> <type> <id>\t<path>"; only file contents matter.
    let blobs: String = listing
        .split('\0')
        .filter_map(|e| e.split('\t').next())
        .filter_map(|meta| match meta.split(' ').collect::<Vec<_>>()[..] {
            [_, "blob", id] => Some(format!("{id}\n")),
            _ => None,
        })
        .collect();
    let suspects: Vec<(String, String)> = keys
        .iter()
        .filter(|(k, _)| found.contains(k))
        .cloned()
        .collect();
    let already = scan_objects(&bare, &blobs, &suspects).await?;
    Ok(found.into_iter().filter(|k| !already.contains(k)).collect())
}

/// Which keys appear in the raw contents of the listed objects (one id per
/// line), streamed through a [`LeakScanner`], not held.
async fn scan_objects(
    bare: &Path,
    objects: &str,
    keys: &[(String, String)],
) -> anyhow::Result<Vec<String>> {
    if objects.trim().is_empty() {
        return Ok(vec![]);
    }
    let mut child = Command::new("git")
        .envs(host_git_env())
        .args(host_git_args())
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("-C")
        .arg(bare)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("could not start git")?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let mut stdout = child.stdout.take().context("no stdout")?;
    let mut stderr = child.stderr.take().context("no stderr")?;
    let mut scanner = LeakScanner::new(keys);
    // Written while the output is read, so neither pipe can fill and stall.
    let ask = async {
        use tokio::io::AsyncWriteExt;
        stdin.write_all(objects.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        drop(stdin);
        std::io::Result::Ok(())
    };
    let scan = async {
        let mut buf = vec![0u8; 64 << 10];
        loop {
            let n = stdout.read(&mut buf).await?;
            if n == 0 {
                return std::io::Result::Ok(());
            }
            scanner.feed(&buf[..n]);
        }
    };
    let mut err = Vec::new();
    let (asked, read, _) = tokio::join!(ask, scan, stderr.read_to_end(&mut err));
    asked?;
    read?;
    let status = child.wait().await?;
    if !status.success() {
        bail!("git cat-file: {}", String::from_utf8_lossy(&err).trim());
    }
    Ok(scanner.found())
}

const MAX_LINE: usize = 1 << 20;
const STDERR_TAIL: usize = 8 << 10;

/// Reads one line, keeping at most `MAX_LINE` bytes of it, so a container
/// cannot make the runner hold an unbounded line in memory. The flag says
/// whether the line was cut.
async fn read_capped_line<R: tokio::io::AsyncBufRead + Unpin>(
    r: &mut R,
) -> std::io::Result<Option<(String, bool)>> {
    read_line_up_to(r, MAX_LINE).await
}

/// [`read_capped_line`] with a cap of its own.
async fn read_line_up_to<R: tokio::io::AsyncBufRead + Unpin>(
    r: &mut R,
    max: usize,
) -> std::io::Result<Option<(String, bool)>> {
    let mut buf = Vec::new();
    let mut seen_any = false;
    let mut cut = false;
    loop {
        let chunk = r.fill_buf().await?;
        if chunk.is_empty() {
            break;
        }
        seen_any = true;
        let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        let room = max.saturating_sub(buf.len());
        cut |= take > room;
        buf.extend_from_slice(&chunk[..take.min(room)]);
        r.consume(take);
        if done {
            break;
        }
    }
    if !seen_any {
        return Ok(None);
    }
    while buf.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
        buf.pop();
    }
    Ok(Some((String::from_utf8_lossy(&buf).into_owned(), cut)))
}

fn docker() -> Command {
    let mut cmd = Command::new("docker");
    cmd.env_clear();
    for k in DOCKER_CLI_ENV {
        if let Ok(v) = std::env::var(k) {
            cmd.env(k, v);
        }
    }
    cmd
}

/// Stops the agent gently first, so Claude Code can save its session for
/// the computer the work moves to, then makes sure its container is gone.
async fn stop_agent(hub: &Hub, id: i64, child: &mut Child) {
    let _ = docker_out(&["stop", "-t", "20", &container_name(id)]).await;
    kill(hub, child, id).await;
}

/// Hands running work to the other computer: stop the agent, keep its
/// unfinished edits as a commit, save a checkpoint and put the work back in
/// the queue for the other runner. Never reports the work as failed: every
/// problem here is logged. If the work could not be saved it is not queued,
/// since the other computer would start from an older copy; it stays here
/// and goes to the owner.
async fn hand_over(
    hub: &Hub,
    data: &Path,
    id: i64,
    wt: &Path,
    branch: &str,
    to: Place,
    child: &mut Child,
) {
    tracing::info!(id, to = to.as_str(), "work was moved; handing it over");
    stop_agent(hub, id, child).await;
    if let Err(err) = save_edits(id, wt).await {
        tracing::warn!(%err, id, "could not save unfinished edits");
    }
    let (state, note) = match save_checkpoint(hub, data, id, wt, branch).await {
        Ok(_) => (ThreadState::Queued, move_note(to)),
        Err(err) => {
            tracing::error!(%err, id, "could not save a checkpoint before moving; keeping the work here");
            (ThreadState::NeedsYou, UNSAVED_MOVE)
        }
    };
    if let Err(err) = with_retry("hand the work over", FINAL_RETRY, async || {
        hub.move_to(id, state, note).await
    })
    .await
    {
        tracing::error!(%err, id, "could not tell the hub where the moved work stands");
    }
}

/// The board note when work could not be saved for a move.
const UNSAVED_MOVE: &str =
    "Could not save the work before moving it, so it stayed here. Try the move again.";

/// The host-side gatekeeper and the address containers reach it at.
#[derive(Clone)]
struct Gate {
    keeper: Gatekeeper,
    url: String,
}

/// Owner messages a step wrote to stdin, and when its last write ended.
type Wrote = (Instant, Vec<i64>);

/// Writes to the agent's stdin from its own task, so an agent that stops
/// reading cannot stall the stdout reader or the hub poll. Reports back the
/// owner messages of each step whose writes all succeeded; [`follow`] acks
/// them once Claude shows it read them. A failed write is not fatal: the
/// container is gone, and what was not written stays undelivered for the
/// next run.
fn spawn_writer(
    thread: i64,
    stdin: ChildStdin,
    wrote: mpsc::UnboundedSender<Wrote>,
) -> (mpsc::UnboundedSender<Step>, JoinHandle<()>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Step>();
    let task = tokio::spawn(async move {
        let mut stdin = Some(stdin);
        while let Some(step) = rx.recv().await {
            let mut written = true;
            for a in step.actions {
                match a {
                    Action::Write(line) => {
                        let ok = match stdin.as_mut() {
                            Some(s) => {
                                s.write_all(line.as_bytes()).await.is_ok()
                                    && s.flush().await.is_ok()
                            }
                            None => false,
                        };
                        if !ok {
                            if stdin.take().is_some() {
                                tracing::warn!(
                                    thread,
                                    "could not write to the agent; it has stopped"
                                );
                            }
                            written = false;
                        }
                    }
                    // Dropping stdin closes it: Claude finishes what it has and exits.
                    Action::Close => stdin = None,
                }
            }
            if written && !step.delivered.is_empty() {
                // Fails only if the run loop is gone; the ids then stay
                // unacked and the hub hands them to the next run.
                let _ = wrote.send((Instant::now(), step.delivered));
            }
        }
    });
    (tx, task)
}

/// Reads the agent's stdout in its own task, a line at a time, into a
/// channel the run loop can wait on alongside the hub poll. Each line comes
/// with when it was read off the pipe (see [`Acks`]).
fn spawn_reader(
    stdout: ChildStdout,
) -> (
    mpsc::Receiver<(Instant, String)>,
    JoinHandle<std::io::Result<()>>,
) {
    // Few lines in flight: each may be up to MAX_LINE.
    let (tx, rx) = mpsc::channel(8);
    let task = tokio::spawn(async move {
        let mut r = BufReader::new(stdout);
        while let Some((line, _)) = read_capped_line(&mut r).await? {
            if tx.send((Instant::now(), line)).await.is_err() {
                break;
            }
        }
        Ok(())
    });
    (rx, task)
}

/// Drains stderr alongside stdout so a chatty container cannot fill the pipe
/// and stall; keeps only its tail. Each line is hidden before it goes in, so
/// trimming the tail cannot cut a test key's value in half.
fn spawn_stderr_tail(stderr: ChildStderr, secrets: Vec<String>) -> JoinHandle<String> {
    tokio::spawn(async move {
        let mut tail = String::new();
        let mut r = BufReader::new(stderr);
        while let Ok(Some((line, cut))) = read_capped_line(&mut r).await {
            let line = if cut {
                hide_cut(&line, &secrets)
            } else {
                hide(&line, &secrets)
            };
            tail.push_str(&line);
            tail.push('\n');
            if tail.len() > STDERR_TAIL {
                let cut = tail.len() - STDERR_TAIL;
                let cut = (cut..tail.len())
                    .find(|i| tail.is_char_boundary(*i))
                    .unwrap_or(0);
                tail.drain(..cut);
            }
        }
        tail
    })
}

enum Ended {
    /// The owner stopped the work while it ran, or it is no longer this
    /// runner's (a cloud worker that was removed).
    Stopped,
    /// The owner moved the work to another computer while it ran.
    Moved(Place),
    /// The agent's output ended. `result` is its last result line, if any,
    /// and `after_result` says the agent printed more after it.
    Exited {
        result: Option<RunEvent>,
        /// The usage-limit report on that last result line, if any (with
        /// the reset time when Claude gave one).
        limit: Option<Option<i64>>,
        after_result: bool,
    },
}

/// How a result shows as progress if it turns out not to be the run's last.
fn progress(e: &RunEvent, from_interrupt: bool) -> Option<RunEvent> {
    match e {
        RunEvent::Finished { ok, summary, .. } => part_done(*ok, summary, from_interrupt),
        _ => None,
    }
}

/// Acks owner messages Claude has read. Ones the hub did not take go back
/// to `acks` for the next try.
async fn ack(hub: &Hub, id: i64, acks: &mut Acks<Instant>, ids: Vec<i64>) {
    let mut failed = Vec::new();
    for batch in ids.chunks(ACK_BATCH) {
        if let Err(err) = hub.delivered(id, batch).await {
            tracing::warn!(%err, id, "could not ack owner messages; trying again later");
            failed.extend_from_slice(batch);
        }
    }
    if !failed.is_empty() {
        acks.failed(failed);
    }
}

/// What the board says when the runner gives up waiting for Claude's next turn.
const GAVE_UP: &str = "Claude went quiet for five minutes while Decurion waited for its \
next step, so Decurion stopped waiting and let the run finish. Any follow-up it had not \
been given yet is kept for the next run.";

/// What a run must hold back: test-key values never reach the hub, and once
/// the gatekeeper has refused calls for the run's money, nothing more goes
/// to the agent.
#[derive(Clone, Copy)]
struct Guard<'a> {
    secrets: &'a [String],
    grant: Option<&'a Grant>,
    /// The run's browser box, told whenever the hub says whether the owner
    /// wants to see it.
    browser: Option<&'a browser_box::Wants>,
}

/// Follows a run until the agent's output ends or the owner stops it: posts
/// what the agent does, passes owner messages to it as the [`Conversation`]
/// decides, and acks them once Claude has read them.
async fn follow(
    hub: &Hub,
    id: i64,
    mut conv: Conversation,
    to_agent: mpsc::UnboundedSender<Step>,
    mut lines: mpsc::Receiver<(Instant, String)>,
    mut wrote: mpsc::UnboundedReceiver<Wrote>,
    guard: Guard<'_>,
) -> Ended {
    let send = |step: Step| {
        if !step.actions.is_empty() || !step.delivered.is_empty() {
            // Fails only if the writer is gone, and then so is the agent.
            let _ = to_agent.send(step);
        }
    };
    let mut poll = tokio::time::interval(POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut hub_down = false;
    let mut acks: Acks<Instant> = Acks::default();
    // Claude Code's outcome is the last result line it prints. Anything after
    // it, or a container that does not exit cleanly, means the agent may have
    // printed a fake result: its cost is then not trusted and the hub charges
    // the whole amount set aside. Earlier results (a follow-up's first part,
    // the turn a steer stopped) are only progress. The held result comes
    // with the progress it shows if it turns out not to be the last.
    let mut result: Option<(RunEvent, Option<RunEvent>)> = None;
    // A usage-limit report is held the same way: an agent could print a fake
    // one to freeze its account, so only Claude's real, final result counts.
    let mut limit: Option<Option<i64>> = None;
    let mut after_result = false;
    // The latest result already shown as progress, and whether anything came
    // after it: the run's outcome if the runner has to give up on the turn
    // that was to follow it.
    let mut last_part: Option<RunEvent> = None;
    let mut after_part = false;
    // A result came and no turn has started since, so Claude is idle. A long
    // tool inside a turn is never cut short by the idle watchdog.
    let mut between_turns = false;
    let mut last_line = Instant::now();
    let mut meter = ContextMeter::default();
    let ended = loop {
        tokio::select! {
            line = lines.recv() => {
                let Some((at, line)) = line else {
                    break Ended::Exited {
                        result: result.map(|(e, _)| e),
                        limit,
                        after_result,
                    };
                };
                last_line = at;
                while let Ok((w, ids)) = wrote.try_recv() {
                    acks.wrote(w, &ids);
                }
                if !line.trim().is_empty() {
                    after_result |= result.is_some();
                    after_part |= last_part.is_some();
                }
                let turn_start = is_turn_start(&line);
                if turn_start {
                    conv.on_turn_start();
                    between_turns = false;
                }
                meter.see(&line);
                let mut events = parse_line_hiding(&line, guard.secrets);
                events.iter_mut().for_each(|e| meter.stamp_event(e));
                let is_result = events.iter().any(|e| matches!(e, RunEvent::Finished { .. }));
                if turn_start || is_result {
                    // Claude has read what was written before this line.
                    let ids = acks.boundary(at);
                    if !ids.is_empty() {
                        ack(hub, id, &mut acks, ids).await;
                    }
                }
                if is_result {
                    limit = None;
                }
                for e in events {
                    let (ok, stop) = match &e {
                        // Out of money or out of usage: nothing more goes to
                        // the agent, whatever is waiting.
                        RunEvent::Finished { ok, summary, .. } => (
                            *ok,
                            summary == BUDGET_STOP
                                || summary == LIMIT_STOP
                                || guard.grant.is_some_and(Grant::used_up),
                        ),
                        RunEvent::LimitHit { reset_epoch } => {
                            limit = Some(*reset_epoch);
                            continue;
                        }
                        _ => {
                            hub.event(id, &e).await;
                            continue;
                        }
                    };
                    if let Some((_, Some(part))) = result.take() {
                        hub.event(id, &part).await;
                    }
                    // Only Claude's own report of a stopped turn is hidden;
                    // any other failure after a steer is shown as an error.
                    let from_interrupt = !ok && is_interrupted_turn(&line, conv.interrupting());
                    let part = progress(&e, from_interrupt);
                    send(conv.on_result(stop));
                    between_turns = true;
                    if conv.closed() {
                        // Nothing more was asked of it: this may be the last.
                        result = Some((e, part));
                        after_result = false;
                    } else {
                        if let Some(part) = part {
                            hub.event(id, &part).await;
                        }
                        last_part = Some(e);
                        after_part = false;
                    }
                }
            }
            _ = poll.tick() => match hub.live(id).await {
                Ok(live) if live.state != ThreadState::Running => break Ended::Stopped,
                Ok(live) if live.place != hub.place => break Ended::Moved(live.place),
                // The hub answers 404 to a cloud worker once the work is not
                // running there; 401 or 403 means this worker was removed.
                Err(err) if lost(&err) => break Ended::Stopped,
                Ok(live) => {
                    hub_down = false;
                    send(conv.on_messages(&live.messages));
                    if let Some(b) = guard.browser {
                        browser_box::want(b, live.browser);
                    }
                }
                // Keep going; the next tick tries again.
                Err(err) => {
                    if !hub_down {
                        tracing::warn!(%err, id, "could not check on the run with the hub");
                    }
                    hub_down = true;
                }
            },
            // Stdin stays open while a turn is expected. If Claude prints
            // nothing at all for this long after a result, that turn is not
            // coming (or Claude is stuck): close stdin so the run can end.
            _ = tokio::time::sleep_until(last_line + IDLE_LIMIT),
                if between_turns && !conv.closed() =>
            {
                tracing::warn!(id, "Claude has been idle too long with input still open; closing it");
                send(conv.give_up());
                hub.event(id, &RunEvent::Message { text: GAVE_UP.to_owned() }).await;
                // Already shown as progress; it is the outcome unless Claude
                // prints another result.
                if let Some(e) = last_part.take() {
                    result = Some((e, None));
                    after_result = after_part;
                }
            }
        }
    };
    // One more try for acks that failed. Messages written but never followed
    // by a turn start or a result stay with the hub for the next run.
    while let Ok((w, ids)) = wrote.try_recv() {
        acks.wrote(w, &ids);
    }
    let ids = acks.last_try();
    if !ids.is_empty() {
        ack(hub, id, &mut acks, ids).await;
    }
    if acks.unread() > 0 {
        tracing::info!(
            id,
            count = acks.unread(),
            "owner messages Claude may not have read stay undelivered for the next run"
        );
    }
    ended
}

/// Follows a pi run (ChatGPT or Grok work) until its output ends or the owner
/// stops or moves it. pi takes no messages while it works: the task and the
/// owner's messages so far went in on stdin, which is then closed, and later
/// messages wait for the next run. Returns how it ended, and whether the
/// runner stopped it at its budget.
///
/// pi has no spending limit of its own, so the runner keeps the running total
/// of what pi reports and removes the container once it reaches the budget.
/// The hub held back part of the money set aside for the call in flight.
async fn follow_pi(
    hub: &Hub,
    id: i64,
    child: &mut Child,
    mut lines: mpsc::Receiver<(Instant, String)>,
    mut wrote: mpsc::UnboundedReceiver<Wrote>,
    budget_usd: Option<f64>,
    secrets: &[String],
) -> (Ended, bool) {
    let mut run = PiRun::hiding(secrets.to_vec());
    let mut stopped = false;
    let mut acks: Acks<Instant> = Acks::default();
    let mut poll = tokio::time::interval(POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut hub_down = false;
    let ended = loop {
        tokio::select! {
            line = lines.recv() => {
                let Some((at, line)) = line else {
                    break None;
                };
                // pi reads all of its task before it prints anything, so any
                // line after the write shows the owner's messages were read.
                while let Ok((w, ids)) = wrote.try_recv() {
                    acks.wrote(w, &ids);
                }
                let ids = acks.boundary(at);
                if !ids.is_empty() {
                    ack(hub, id, &mut acks, ids).await;
                }
                let events = run.parse_line(&line);
                if stopped {
                    // Still counted: money spent on a call that was in flight.
                    continue;
                }
                for e in &events {
                    hub.event(id, e).await;
                }
                if run.must_stop(budget_usd) {
                    stopped = true;
                    tracing::info!(id, cost = run.cost_usd(), "stopping a run at its budget");
                    // pi keeps spending until its container is gone.
                    kill(hub, child, id).await;
                }
            }
            _ = poll.tick() => match hub.live(id).await {
                Ok(live) if live.state != ThreadState::Running => break Some(Ended::Stopped),
                Ok(live) if live.place != hub.place => break Some(Ended::Moved(live.place)),
                Err(err) if lost(&err) => break Some(Ended::Stopped),
                Ok(_) => hub_down = false,
                Err(err) => {
                    if !hub_down {
                        tracing::warn!(%err, id, "could not check on the run with the hub");
                    }
                    hub_down = true;
                }
            },
        }
    };
    let ids = acks.last_try();
    if !ids.is_empty() {
        ack(hub, id, &mut acks, ids).await;
    }
    if let Some(ended) = ended {
        return (ended, false);
    }
    let result = if stopped {
        Some(run.budget_stop())
    } else {
        run.finished()
    };
    // An output pi could not have its spending read from is not a budget stop.
    let budget_stop = stopped
        && matches!(&result, Some(RunEvent::Finished { summary, .. }) if summary == BUDGET_STOP);
    let ended = Ended::Exited {
        result,
        limit: None,
        after_result: false,
    };
    (ended, budget_stop)
}

/// Whether the hub said the running work is no longer this runner's.
fn lost(err: &anyhow::Error) -> bool {
    matches!(
        err.downcast_ref::<HubError>(),
        Some(HubError::Refused(
            _,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND,
            _,
        ))
    )
}

/// Removes a thread's container, and its browser box with it, and checks
/// that they are really gone.
async fn remove_container(id: i64) -> bool {
    remove_named(id, &[container_name(id), browser_container_name(id)]).await
}

/// Removes containers by name and checks that they are really gone. Tried a
/// few times: docker refuses to remove a container it is still taking down.
async fn remove_named(id: i64, names: &[String]) -> bool {
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let mut rm = vec!["rm", "-f"];
        rm.extend(names.iter().map(String::as_str));
        match docker_out(&rm).await {
            // Fails when there is no such container too; the check says.
            Ok(out) if !out.status.success() => tracing::debug!(
                id,
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "docker rm failed"
            ),
            Ok(_) => {}
            Err(err) => tracing::warn!(%err, id, "could not run docker rm"),
        }
        // Several name filters match any of them.
        let filters: Vec<String> = names.iter().map(|n| format!("name=^{n}$")).collect();
        let mut ps = vec!["ps", "-aq"];
        for f in &filters {
            ps.extend(["--filter", f.as_str()]);
        }
        match docker_out(&ps).await {
            Ok(out) if out.status.success() && out.stdout.trim_ascii().is_empty() => return true,
            Ok(out) if !out.status.success() => tracing::warn!(
                id,
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "could not check that the container is gone"
            ),
            Ok(_) => {}
            Err(err) => tracing::warn!(%err, id, "could not run docker ps"),
        }
    }
    false
}

/// A docker command's output, given up on if docker hangs.
async fn docker_out(args: &[&str]) -> anyhow::Result<std::process::Output> {
    let out = tokio::time::timeout(
        Duration::from_secs(30),
        docker().args(args).kill_on_drop(true).output(),
    )
    .await
    .context("docker did not answer")??;
    Ok(out)
}

/// Stops a run's container now, whatever the agent is doing. If docker
/// cannot be made to remove it, the owner is told to: the agent may still be
/// working with nobody watching.
async fn kill(hub: &Hub, child: &mut Child, id: i64) {
    let gone = remove_container(id).await;
    let _ = child.kill().await;
    if !gone {
        let name = container_name(id);
        tracing::error!(id, container = %name, "could not stop the agent's container");
        let text = format!(
            "Decurion could not stop this agent's box (the Docker container named {name}), \
so it may still be working. Stop it yourself: open Docker Desktop, find {name} under \
Containers, and delete it."
        );
        hub.event(id, &RunEvent::Message { text }).await;
    }
}

/// Runs one claimed thread behind its site gate, which lets the box reach
/// only the websites the app allows.
async fn run(
    hub: &Hub,
    data: &Path,
    gate: &Gate,
    c: Claim,
    keys: &[(String, String)],
    states: Vec<SiteState>,
    site_secrets: Vec<String>,
) -> anyhow::Result<()> {
    // A pay-per-use box reaches the gatekeeper through its site gate, which
    // passes that one address through and nothing else on the PC. The
    // websites of the app's add-on tools join the list for this run only;
    // pi runs get no add-on tools.
    let mut app = c.app.clone();
    if c.thread.ai == decurion_proto::Ai::Claude {
        for s in decurion_runner::addons::sites(&c.addons) {
            if !app.sites.contains(&s) {
                app.sites.push(s);
            }
        }
    }
    let exit = start_site_gate(
        hub,
        BoxNet::work(c.thread.id),
        &app,
        Some(c.thread.id),
        gate.relay(Credential::for_account(c.account)).as_deref(),
    )
    .await?;
    let id = c.thread.id;
    let outcome = run_in_box(hub, data, gate, c, keys, states, site_secrets).await;
    // Tool boxes sit on the run's network as boxes of their own: remove any
    // an early error left, or the network cannot be removed with the gate.
    addon_box::clear(data, id).await;
    exit.stop().await;
    outcome
}

/// Runs one claimed thread to the end and reports the outcome. `keys` are the
/// app's test keys: they go into the container, and their values are hidden
/// from everything sent to the hub. `states` are the app's saved website
/// sign-ins, opened on this computer: they go into the run's browser box
/// only, never the agent's, and `site_secrets` are their values, hidden like
/// test keys.
async fn run_in_box(
    hub: &Hub,
    data: &Path,
    gate: &Gate,
    c: Claim,
    keys: &[(String, String)],
    states: Vec<SiteState>,
    site_secrets: Vec<String>,
) -> anyhow::Result<()> {
    let id = c.thread.id;
    // The copy of each saved sign-in this attempt holds, for its reports.
    let given: std::collections::BTreeMap<i64, i64> =
        c.sign_ins.iter().map(|r| (r.id, r.version)).collect();
    let session = c
        .thread
        .session_id
        .clone()
        .context("claim has no session")?;
    let branch = c.thread.branch.clone().context("claim has no branch")?;
    // Only one copy of a thread may ever run: clear any container left behind
    // by a runner that died, and do not start another while it may be there.
    if !remove_container(id).await {
        bail!(
            "could not remove the Docker container {} left from an earlier run; \
make sure Docker Desktop is running and delete that container there",
            container_name(id)
        );
    }
    // A save or restore box left by a runner that died.
    let _ = docker_out(&["rm", "-f", &tool_container_name(id)]).await;

    let (wt, rebuilt) = workspace(hub, data, &c, &branch).await?;
    // A research run's report comes from this run alone: one left by an
    // earlier try is removed before every start, so it is never sent.
    if c.thread.research {
        clear_report(&wt).context("could not remove the old research report")?;
    }
    // The home volume is ready (sessions restored); the owner's files go in
    // fresh on every start.
    let xfer = transfer_dir(data, id)?;
    put_context(
        hub,
        &c.context,
        &format!("/runner/threads/{id}/context"),
        &xfer,
        &tool_container_name(id),
        &home_volume(id),
    )
    .await?;
    let _ = remove_all(&xfer);
    let start = Start::pick(c.resume, rebuilt, c.checkpoint.is_some());
    let cred = Credential::for_account(c.account);
    let pi = cred.pi_model().is_some();
    if pi && c.budget_usd.is_none() {
        // The runner is pi's only spending limit.
        bail!("{} work came without a budget", c.account.label());
    }
    if cred == Credential::PayPerUse && !gate.keeper.is_alive() {
        // Without the gatekeeper the run could only fail, and a failed
        // pay-per-use run is charged its whole budget. Stop before starting.
        bail!("the gatekeeper has stopped; restart the runner before running pay-per-use work");
    }
    let token = credential(hub, id, cred).await?;
    let mut cmd = docker();
    // A pay-per-use container gets a token for the gatekeeper, never the key.
    // The grant is revoked when it drops, on every way out of this function.
    let (grant, values) = match cred {
        Credential::PayPerUse => {
            // The gatekeeper prices every answer and stops the run's calls
            // once its set-aside money is used.
            let budget = c.reserved_micros.map(|m| m.max(0) as u64);
            let g = gate.keeper.grant_with_budget(token, budget);
            let v = vec![g.token().to_owned(), RELAY_URL.to_owned()];
            (Some(g), v)
        }
        Credential::Max(_) | Credential::OpenAi | Credential::Xai => (None, vec![token]),
    };
    // Names come from the same list the docker arguments pass with `-e`.
    anyhow::ensure!(
        values.len() == cred.vars().len(),
        "credential variables out of step"
    );
    for (name, value) in cred.vars().iter().zip(values) {
        cmd.env(name, value);
    }
    for (name, value) in keys {
        cmd.env(name, value);
    }
    let mut secrets: Vec<String> = keys.iter().map(|(_, v)| v.clone()).collect();
    secrets.extend(site_secrets.iter().cloned());
    // A sign-in value turning up in the agent's commits stops the work the
    // same way a test key does.
    let mut scanned = keys.to_vec();
    scanned.extend(
        site_secrets
            .into_iter()
            .map(|v| ("(a saved website sign-in)".to_owned(), v)),
    );
    let mut prompt = prompt_for(
        &c.thread.prompt,
        start,
        c.thread.research,
        &c.test_keys,
        &c.messages,
        c.interrupted.as_ref(),
    );
    if let Some(more) = context_prompt(&c.context) {
        prompt.push_str("\n\n");
        prompt.push_str(&more);
    }
    if let Some(more) = patient_data_prompt(&c.app) {
        prompt.push_str("\n\n");
        prompt.push_str(more);
    }
    if !pi {
        if let Some(more) = sign_ins::sign_ins_prompt(&c.sign_ins, &states) {
            prompt.push_str("\n\n");
            prompt.push_str(&more);
        }
    }
    // A Claude Code run's test browser goes in a box of its own, up and
    // checked before the agent's box starts and joins its network. The app's
    // saved sign-ins go into that box only. pi runs have no browser box.
    // Removed with the agent's box (see remove_container), and on every
    // other way out of this function when `browser` drops.
    let browser = if pi {
        None
    } else {
        Some(browser_box::start_run(hub, id, states, given, &secrets).await?)
    };
    // The app's add-on tools, each in a box of its own on the browser box's
    // network, never in the agent's. pi runs get none.
    let tools = if pi {
        None
    } else {
        let t = addon_box::start(hub, data, id, &network_name(id), &c.addons, keys, &secrets).await;
        if let Some(more) = &t.prompt {
            prompt.push_str("\n\n");
            prompt.push_str(more);
        }
        Some(t)
    };
    // Tell the hub before launching: from here the run may cost money, and if
    // the hub cannot be told, the run does not start.
    hub.post(&format!("/threads/{id}/events"), &RunEvent::Started)
        .await
        .context("could not tell the hub the run is starting")?;
    let args = if pi {
        pi_run_args(id, &wt, &session, cred, &c.test_keys)?
    } else {
        let mut a = run_args(
            id,
            &wt,
            &session,
            start.resumes(),
            cred,
            c.budget_usd,
            &c.test_keys,
        )?;
        if let Some(config) = tools.as_ref().and_then(|t| t.config.as_deref()) {
            decurion_runner::addons::add_to_run(&mut a, config)?;
        }
        a
    };
    let mut child = cmd
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("could not start docker")?;

    let (wrote_tx, wrote) = mpsc::unbounded_channel();
    let (to_agent, writer) = spawn_writer(id, child.stdin.take().context("no stdin")?, wrote_tx);
    let stderr_task = spawn_stderr_tail(child.stderr.take().context("no stderr")?, secrets.clone());
    let (lines, reader) = spawn_reader(child.stdout.take().context("no stdout")?);

    let mut budget_stopped = false;
    let ended = if pi {
        // pi gets its task once, as plain text, and stdin closes behind it.
        let _ = to_agent.send(Step {
            actions: vec![Action::Write(prompt), Action::Close],
            delivered: c.messages.iter().map(|m| m.id).collect(),
        });
        drop(to_agent);
        let (ended, stopped) =
            follow_pi(hub, id, &mut child, lines, wrote, c.budget_usd, &secrets).await;
        budget_stopped = stopped;
        ended
    } else {
        // Stdin stays open so owner messages can follow the prompt. The
        // claim's undelivered messages are in the prompt and are acked once
        // Claude has read it.
        let (conv, first) = Conversation::start(&prompt, &c.messages);
        let _ = to_agent.send(first);
        let guard = Guard {
            secrets: &secrets,
            grant: grant.as_ref(),
            browser: browser.as_ref().map(|b| b.wants()),
        };
        follow(hub, id, conv, to_agent, lines, wrote, guard).await
    };
    let (result, limit, after_result) = match ended {
        Ended::Stopped => {
            // The owner already moved it; the hub has the last word. Nothing
            // is saved: the work is not this runner's to touch any more.
            tracing::info!(id, "stopped by the owner");
            kill(hub, &mut child, id).await;
            browser_box::stop(browser).await;
            if let Some(t) = &tools {
                addon_box::stop(t).await;
            }
            let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
            return Ok(());
        }
        Ended::Moved(to) => {
            hand_over(hub, data, id, &wt, &branch, to, &mut child).await;
            browser_box::stop(browser).await;
            if let Some(t) = &tools {
                addon_box::stop(t).await;
            }
            let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
            return Ok(());
        }
        Ended::Exited {
            result,
            limit,
            after_result,
        } => (result, limit, after_result),
    };
    reader.await.context("stdout reader")??;
    let status = child.wait().await?;
    // The agent is done with its browser and its add-on tools.
    browser_box::stop(browser).await;
    if let Some(t) = &tools {
        addon_box::stop(t).await;
    }
    // The container is gone; its token goes with it.
    // For pay-per-use, the gatekeeper's count is the cost: it saw every call,
    // including any the agent made itself, and runs outside the container.
    // Answers still finishing are waited for, so their cost is in; if they do
    // not finish in time the count is not trusted and the hub charges the
    // whole set-aside amount.
    let metered_usd = match &grant {
        Some(g) if g.finish(Duration::from_secs(30)).await => {
            Some((c.prior_micros as u64 + g.spent_micros()) as f64 / 1_000_000.0)
        }
        _ => None,
    };
    // Out of the run's money: by the gatekeeper's count, or by the runner's
    // own for pi.
    let used_up = grant.as_ref().is_some_and(|g| g.used_up()) || budget_stopped;
    let pay_per_use = grant.is_some();
    drop(grant);
    // The container is gone, so the writer has nothing left to do.
    let _ = writer.await;
    let finished = match result {
        Some(RunEvent::Finished {
            ok,
            summary,
            cost_usd,
            input_tokens,
            output_tokens,
            tokens,
        }) => {
            // The runner stopped a pi run at its budget itself, so its
            // container does not exit cleanly, but what pi reported until
            // then stands.
            let trusted = (status.success() || budget_stopped) && !after_result;
            let reported = cost_usd.filter(|_| trusted);
            // The higher of the gatekeeper's count and Claude's own report;
            // a pay-per-use run whose count is unknown reports nothing.
            let cost = if pay_per_use {
                metered_usd.map(|m| reported.map_or(m, |r| m.max(r)))
            } else {
                reported
            };
            let summary = if !ok && used_up {
                BUDGET_STOP.to_owned()
            } else {
                summary
            };
            hub.event(
                id,
                &RunEvent::Finished {
                    ok,
                    summary: summary.clone(),
                    cost_usd: cost,
                    input_tokens,
                    output_tokens,
                    tokens,
                },
            )
            .await;
            Some((ok, summary))
        }
        _ => {
            // No result line at all: still report what the gatekeeper saw,
            // and say so if the run was stopped by its budget.
            let summary = if used_up {
                BUDGET_STOP.to_owned()
            } else {
                String::new()
            };
            if let Some(cost) = metered_usd {
                hub.event(
                    id,
                    &RunEvent::Finished {
                        ok: false,
                        summary: summary.clone(),
                        cost_usd: Some(cost),
                        input_tokens: None,
                        output_tokens: None,
                        tokens: None,
                    },
                )
                .await;
            }
            used_up.then_some((false, summary))
        }
    };
    let max_limit = limit.filter(|_| !after_result && matches!(cred, Credential::Max(_)));
    let err = stderr_task.await.unwrap_or_default();
    let exit = Exit {
        status,
        stderr: &err,
        pay_per_use,
        used_up,
    };
    let (to, note) = outcome(data, &wt, &c, finished, max_limit, exit, &scanned).await;
    // Save the work whatever the outcome, so it can continue anywhere later.
    // A failed save is logged and the outcome is still reported, except that
    // unsaved work never goes back in the queue, where another computer could
    // pick it up from an older copy: it goes to the owner instead.
    let (to, note) = match save_checkpoint(hub, data, id, &wt, &branch).await {
        Ok(_) => (to, note),
        Err(err) => {
            tracing::error!(%err, id, "could not save a checkpoint");
            // Only a Max usage limit queues work at the end of a run.
            if to == ThreadState::Queued {
                (
                    ThreadState::NeedsYou,
                    "The Max account is out of usage, and the work could not be saved, so it \
                     stayed here instead of waiting in the queue. Queue it again once the \
                     account resets."
                        .to_owned(),
                )
            } else {
                (to, note)
            }
        }
    };
    // A research run goes to review only with its report at the hub, so the
    // owner never finds it there with nothing to read.
    let (to, note) = if c.thread.research && to == ThreadState::InReview {
        match send_report(hub, id, &wt, &secrets).await {
            Ok(()) => (to, note),
            Err(err) => {
                tracing::error!(%err, id, "could not send the research report");
                (
                    ThreadState::Failed,
                    format!("the research report could not be sent to the hub: {err:#}"),
                )
            }
        }
    } else {
        (to, note)
    };
    let note = hide(&note, &secrets);
    with_retry("report the outcome", FINAL_RETRY, async || {
        if let (ThreadState::Queued, Some(reset_epoch)) = (to, max_limit) {
            // The hub saves the outage and requeues the work in one step.
            hub.post(
                &format!("/threads/{id}/events"),
                &RunEvent::LimitHit { reset_epoch },
            )
            .await?;
            Ok(())
        } else {
            hub.move_to(id, to, &note).await
        }
    })
    .await
}

/// How the container ended: its exit status, the tail of its stderr, and
/// whether the gatekeeper ran out of the run's money.
struct Exit<'a> {
    status: ExitStatus,
    stderr: &'a str,
    /// A pay-per-use run, and whether the gatekeeper stopped it for money.
    pay_per_use: bool,
    used_up: bool,
}

/// Sends a research run's report to the hub, with test keys and saved
/// sign-in values hidden like all other output. With no report file the hub
/// makes a page from the agent's summary instead.
async fn send_report(hub: &Hub, id: i64, wt: &Path, secrets: &[String]) -> anyhow::Result<()> {
    // Hiding can make the page longer; one past the limit is not sent, and
    // the owner gets the summary's page instead.
    let html = read_report(wt)
        .map(|page| hide(&page, secrets))
        .filter(|page| page.len() <= MAX_PAGE_BYTES);
    let report = ResearchReport { html };
    with_retry("send the research report", FINAL_RETRY, async || {
        hub.post(&format!("/runner/threads/{id}/report"), &report)
            .await?;
        Ok(())
    })
    .await
}

/// Where the work goes after the run. The agent's own "done" is not enough:
/// work only goes to review if it actually left commits on its branch, and
/// none of them holds a test key. A research run needs no commits, only a
/// report or a summary; anything it commits is ignored and never sent on.
async fn outcome(
    data: &Path,
    wt: &Path,
    c: &Claim,
    finished: Option<(bool, String)>,
    max_limit: Option<Option<i64>>,
    exit: Exit<'_>,
    keys: &[(String, String)],
) -> (ThreadState, String) {
    let Exit {
        status,
        stderr,
        pay_per_use,
        used_up,
    } = exit;
    let branch = c.thread.branch.as_deref().unwrap_or_default();
    match finished {
        Some((true, summary)) if status.success() && c.thread.research => {
            research_outcome(read_report(wt).is_some(), &summary)
        }
        Some((true, _)) if status.success() => {
            let merge_from = c.merge_from.as_deref();
            if let Some(m) = merge_from {
                match merged_main(data, wt, branch, m).await {
                    Ok(true) => {}
                    Ok(false) => return (ThreadState::Failed, NOT_MERGED.to_owned()),
                    Err(e) => {
                        return (
                            ThreadState::Failed,
                            format!("could not check how main was merged: {e}"),
                        )
                    }
                }
            }
            checked_commits(data, wt, branch, merge_from, keys).await
        }
        Some((true, summary)) => (
            ThreadState::Failed,
            format!("container exited with {status} after the agent reported: {summary}"),
        ),
        // The Max account is out: back to the queue, where it waits for the
        // reset (or the owner moves it to pay-per-use). Only for a Max run,
        // and only when the report was Claude's last line; anything else
        // goes to the owner rather than looping.
        Some((false, summary)) if summary == LIMIT_STOP => {
            if max_limit.is_some() {
                (ThreadState::Queued, summary)
            } else {
                (
                    ThreadState::NeedsYou,
                    "Claude reported a usage limit that Decurion could not confirm; check the account."
                        .to_owned(),
                )
            }
        }
        // Out of money for this run, not broken: the owner can give it more.
        // Decided by the gatekeeper's count, or by Claude Code's own limit on
        // a pay-per-use run; never by text alone on a Max run.
        Some((false, summary)) if used_up || (pay_per_use && summary == BUDGET_STOP) => {
            (ThreadState::NeedsYou, BUDGET_STOP.to_owned())
        }
        Some((false, summary)) => (ThreadState::Failed, summary),
        None => (
            ThreadState::Failed,
            format!(
                "container ended without a result ({status}): {}",
                redact(stderr.trim())
            ),
        ),
    }
}

/// Said when a Bring up to date run did not merge main as it was asked.
const NOT_MERGED: &str = "The update did not merge main in the expected way, so it was not \
sent for review. Retry it, and the agent will merge decurion-main into the branch with one \
plain merge.";

/// Where finished work with commits goes: review if it saved any and none
/// holds a test key, otherwise failed with the reason.
async fn checked_commits(
    data: &Path,
    wt: &Path,
    branch: &str,
    merge_from: Option<&str>,
    keys: &[(String, String)],
) -> (ThreadState, String) {
    match new_commits(data, wt, branch, merge_from).await {
        Ok(0) => (
            ThreadState::Failed,
            "agent said it finished but saved no commits".to_owned(),
        ),
        Ok(n) => match leaked_keys(data, wt, branch, merge_from, keys).await {
            Ok(found) if found.is_empty() => (
                ThreadState::InReview,
                format!("agent finished with {n} new commit(s); ready for review"),
            ),
            Ok(found) => (
                ThreadState::Failed,
                format!(
                    "found the test key{} {} in the agent's commits, so the work was not \
sent for review. Retry it and the agent will be asked to rewrite its branch without {}.",
                    if found.len() == 1 { "" } else { "s" },
                    found.join(", "),
                    if found.len() == 1 { "it" } else { "them" },
                ),
            ),
            // Fail closed: unchecked work does not go to review.
            Err(e) => (
                ThreadState::Failed,
                format!("could not check commits for test keys: {e}"),
            ),
        },
        Err(e) => (ThreadState::Failed, format!("could not check commits: {e}")),
    }
}

/// Marks a run that could not finish as failed.
async fn fail(hub: &Hub, id: i64, note: &str) {
    hub.event(
        id,
        &RunEvent::Finished {
            ok: false,
            summary: note.to_owned(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            tokens: None,
        },
    )
    .await;
    let _ = with_retry("report the failure", FINAL_RETRY, async || {
        hub.move_to(id, ThreadState::Failed, note).await
    })
    .await;
}

/// Variables set on the docker process for a container to take with `-e`.
type ContainerEnv = Vec<(&'static str, String)>;

impl Gate {
    /// Where a box's site gate relays to: the gatekeeper, for Claude
    /// pay-per-use boxes only. That one address is all of the PC the gate passes through.
    fn relay(&self, cred: Credential) -> Option<String> {
        (cred == Credential::PayPerUse).then(|| self.url.trim_start_matches("http://").to_owned())
    }

    /// A helper container's credential. A pay-per-use container gets a token
    /// for the gatekeeper, held to the money set aside for the job, never the
    /// key; the grant is revoked when it drops, so the caller keeps it until
    /// the container is gone.
    fn credential(
        &self,
        cred: Credential,
        reserved_micros: Option<i64>,
    ) -> anyhow::Result<(Option<Grant>, ContainerEnv)> {
        if cred == Credential::PayPerUse && !self.keeper.is_alive() {
            // Without the gatekeeper the job could only fail, and a failed
            // pay-per-use job is charged its whole budget. Stop before starting.
            bail!("the gatekeeper has stopped; restart the runner before running pay-per-use work");
        }
        let token = secret(cred)?;
        let (grant, values) = match cred {
            Credential::PayPerUse => {
                let budget = reserved_micros.map(|m| m.max(0) as u64);
                let g = self.keeper.grant_with_budget(token, budget);
                // The box reaches the gatekeeper only through its site gate.
                let v = vec![g.token().to_owned(), RELAY_URL.to_owned()];
                (Some(g), v)
            }
            Credential::Max(_) | Credential::OpenAi | Credential::Xai => (None, vec![token]),
        };
        // Names come from the same list helper_run_args passes with `-e`.
        anyhow::ensure!(
            values.len() == cred.vars().len(),
            "credential variables out of step"
        );
        Ok((grant, cred.vars().iter().copied().zip(values).collect()))
    }
}

/// Where a helper job's code lives on the host and what happens to it after.
struct HelperBox {
    dir: PathBuf,
    read_only: bool,
    home_volume: String,
    /// Deleted after the job, home volume included.
    throwaway: bool,
    /// Mounted at /out for jobs that hand back a page; always deleted.
    out: Option<PathBuf>,
}

impl HelperBox {
    fn for_job(data: &Path, job: &HelperClaim) -> Self {
        let id = job.job_id;
        let app = &job.app.name;
        let (dir, read_only, home_volume, throwaway) = match &job.source {
            HelperSource::App { goal_id, .. } => (
                data.join("goals").join(format!("{app}-{goal_id}")),
                true,
                goal_home_volume(*goal_id),
                false,
            ),
            HelperSource::AppCopy { goal_id, .. } => (
                data.join("designs")
                    .join(format!("{app}-{goal_id}-job{id}")),
                false,
                design_home_volume(id),
                true,
            ),
            HelperSource::Branch { thread_id, .. } => (
                data.join("checks")
                    .join(format!("{app}-{thread_id}-job{id}")),
                false,
                check_home_volume(id),
                true,
            ),
        };
        let out = job
            .kind
            .makes_page()
            .then(|| data.join("out").join(format!("job{id}")));
        Self {
            dir,
            read_only,
            home_volume,
            throwaway,
            out,
        }
    }

    /// Deletes what this job leaves behind. An agent has written in these
    /// folders, so they are only ever deleted, never looked into with git.
    async fn clean(&self) {
        if self.throwaway {
            remove_dir(&self.dir);
            let _ = docker_out(&["volume", "rm", "-f", &self.home_volume]).await;
        }
        if let Some(out) = &self.out {
            remove_dir(out);
        }
    }
}

/// Best-effort delete. Does not follow links the agent may have planted.
fn remove_dir(dir: &Path) {
    if let Err(err) = std::fs::remove_dir_all(dir) {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(%err, dir = %dir.display(), "could not delete");
        }
    }
}

/// The coordinator's copy of the app, made on the goal's first turn and kept
/// for the rest of it. It is mounted read-only, so the agent never writes it.
async fn goal_copy(app: &App, dir: &Path) -> anyhow::Result<()> {
    if dir.join(".git").is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dir.parent().context("goal copy parent")?)?;
    // Not `with_extension`: an app named like `shop.web` would land on
    // `shop.partial`, which another app's copy may be using.
    let tmp = sibling(dir, "partial");
    remove_all(&tmp)?;
    clone_into(&app.repo_path, &tmp).await?;
    // Only a finished clone gets the real name, as in `workspace`.
    std::fs::rename(&tmp, dir)?;
    Ok(())
}

/// Clones the hub's bundle of a goal's start commit into the new folder
/// `dir` and leaves it detached at exactly `want`, which the hub must have
/// named as the bundle's commit (`sent`). A bundle is a plain file, so the
/// copy shares no objects with anything. Any mismatch is an error, never a
/// fall back to the owner's folder: the goal was planned against `want`.
/// Only a finished copy gets the name `dir`.
async fn copy_from_bundle(
    bundle: &Path,
    sent: Option<&str>,
    want: &str,
    dir: &Path,
) -> anyhow::Result<()> {
    if !is_sha(want) {
        bail!("the hub sent an odd start commit {want:?}");
    }
    if sent != Some(want) {
        bail!("the hub's bundle is not of the goal's start commit");
    }
    if !bundle.is_file() {
        bail!("the hub's bundle of the start commit is missing");
    }
    std::fs::create_dir_all(dir.parent().context("helper copy parent")?)?;
    let tmp = sibling(dir, "partial");
    remove_all(&tmp)?;
    let r = async {
        let bundle_s = bundle.to_string_lossy().into_owned();
        let tmp_s = tmp.to_string_lossy().into_owned();
        git(
            Path::new("."),
            &[
                "clone",
                "--quiet",
                "--no-checkout",
                "--no-hardlinks",
                "--",
                &bundle_s,
                &tmp_s,
            ],
        )
        .await?;
        git(&tmp, &["checkout", "--quiet", "--detach", want]).await?;
        // The bundle file is about to be deleted; leave no remote pointing at it.
        git(&tmp, &["remote", "remove", "origin"]).await?;
        let head = git(&tmp, &["rev-parse", "HEAD"]).await?;
        anyhow::ensure!(head == want, "the copy is at {head}, not {want}");
        anyhow::Ok(())
    }
    .await;
    if r.is_err() {
        let _ = remove_all(&tmp);
        return r;
    }
    std::fs::rename(&tmp, dir)?;
    Ok(())
}

/// A helper's copy of the app in `dir`, at the goal's start commit `start`
/// the job was claimed with. With `keep`, a copy already there at exactly
/// `start` is used as it is (the project lead's, kept across turns).
/// Anything else in `dir`, such as what an earlier try of the same job
/// left, is cleared first: the finished copy could never take its name
/// otherwise.
async fn copy_at_start(
    hub: &Hub,
    data: &Path,
    job_id: i64,
    goal_id: i64,
    start: &str,
    dir: &Path,
    keep: bool,
) -> anyhow::Result<()> {
    if keep && copy_is_at(dir, start) {
        return Ok(());
    }
    remove_all(dir)?;
    helper_copy_at(hub, data, job_id, goal_id, start, dir).await
}

/// A helper's copy of the app at a goal's start commit `start`, from the
/// hub's bundle of exactly that commit (`/runner/goals/{id}/base?at=`),
/// even if a later round has moved the goal on since the claim. Never from
/// the owner's folder.
async fn helper_copy_at(
    hub: &Hub,
    data: &Path,
    job_id: i64,
    goal_id: i64,
    start: &str,
    dir: &Path,
) -> anyhow::Result<()> {
    if !is_sha(start) {
        bail!("the hub sent an odd start commit {start:?}");
    }
    let xfer = data.join("transfer").join(format!("helper-base-{job_id}"));
    remove_all(&xfer)?;
    std::fs::create_dir_all(&xfer)?;
    let r = async {
        let bundle = xfer.join("base.bundle");
        let headers = download_retrying(
            hub,
            &format!("/runner/goals/{goal_id}/base?at={start}"),
            &bundle,
            CODE_CAP,
        )
        .await?;
        let sent = headers.get("x-decurion-base").and_then(|v| v.to_str().ok());
        copy_from_bundle(&bundle, sent, start, dir).await
    }
    .await;
    let _ = remove_all(&xfer);
    r
}

/// Brings this PC's review copy of a thread's branch up to the hub's latest
/// saved copy, so a check sees the code even when another computer (the
/// cloud worker) finished the work. Nothing to do when this PC made that
/// saved copy itself: its run already put its commits in the review copy.
/// Only the review copy and the starting-commit file change; the work's own
/// folder is rebuilt by its next run here, as for any move.
async fn sync_review_copy(
    hub: &Hub,
    data: &Path,
    app: &App,
    thread_id: i64,
    branch: &str,
    checkpoint: i64,
    xfer: &Path,
) -> anyhow::Result<()> {
    let ws = data
        .join("workspaces")
        .join(format!("{}-{thread_id}", app.name));
    let bare = review_repo(data, &ws)?;
    let made_here = std::fs::read_to_string(sibling(&ws, "checkpoint"))
        .is_ok_and(|t| t.trim() == checkpoint.to_string());
    if made_here && bare.is_dir() && base_file(&ws).is_file() {
        return Ok(());
    }
    tracing::info!(
        thread_id,
        checkpoint,
        "bringing in work saved on another computer for its check"
    );
    // Its own folder (the caller's): the work itself may be running here
    // meanwhile, and a check and a preview of it may both be bringing it in.
    remove_all(xfer)?;
    std::fs::create_dir_all(xfer)?;
    let r = async {
        let base_bundle = xfer.join("base.bundle");
        let headers = download_retrying(
            hub,
            &format!("/runner/threads/{thread_id}/base"),
            &base_bundle,
            CODE_CAP,
        )
        .await?;
        let base = headers
            .get("x-decurion-base")
            .and_then(|v| v.to_str().ok())
            .filter(|s| is_sha(s))
            .context("hub sent no starting commit")?
            .to_owned();
        let code = xfer.join("code.bundle");
        download_retrying(
            hub,
            &format!("/runner/threads/{thread_id}/checkpoint/{checkpoint}/code"),
            &code,
            CODE_CAP,
        )
        .await?;
        if !bare.exists() {
            std::fs::create_dir_all(&bare)?;
            git(&bare, &["init", "--quiet", "--bare"]).await?;
        }
        let base_s = base_bundle.to_string_lossy().into_owned();
        let code_s = code.to_string_lossy().into_owned();
        let refspec = format!("+refs/heads/{branch}:refs/heads/{branch}");
        // The starting commit first: the saved code is only what came after.
        git(
            &bare,
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                "--",
                &base_s,
                "+refs/heads/decurion-base:refs/decurion/base",
            ],
        )
        .await?;
        git(
            &bare,
            &["fetch", "--quiet", "--no-tags", "--", &code_s, &refspec],
        )
        .await?;
        std::fs::create_dir_all(ws.parent().context("workspace parent")?)?;
        write_atomic(&base_file(&ws), base.as_bytes(), false)?;
        anyhow::Ok(())
    }
    .await;
    let _ = remove_all(xfer);
    r
}

/// A download for a helper job (a check's code, a job's attached files),
/// tried again over a short hiccup. A plain loop rather than [`with_retry`]:
/// the helper loop runs on its own task, which the async closures there
/// cannot be sent to.
async fn download_retrying(
    hub: &Hub,
    path: &str,
    dest: &Path,
    cap: u64,
) -> anyhow::Result<HeaderMap> {
    let start = Instant::now();
    let mut attempt = 0;
    loop {
        let err = match hub.download(path, dest, cap).await {
            Ok(h) => return Ok(h),
            Err(e) => e,
        };
        let wait = backoff(attempt);
        let again = err
            .downcast_ref::<HubError>()
            .is_some_and(HubError::retriable);
        if !again || start.elapsed() + wait > DOWNLOAD_RETRY {
            return Err(err);
        }
        tokio::time::sleep(wait).await;
        attempt += 1;
    }
}

/// A checker's copy of a thread's branch, cloned from the runner's own review
/// repo rather than the building agent's folder, whose git config and hooks
/// that agent controls.
async fn branch_copy(
    data: &Path,
    app: &App,
    thread_id: i64,
    branch: &str,
    dir: &Path,
) -> anyhow::Result<()> {
    if !valid_branch(branch) {
        bail!("refusing odd branch name {branch:?}");
    }
    let ws = data
        .join("workspaces")
        .join(format!("{}-{thread_id}", app.name));
    let base = std::fs::read_to_string(base_file(&ws)).context("missing base commit")?;
    let base = base.trim();
    if base.is_empty() || !base.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("base commit of thread {thread_id} is not a commit id");
    }
    let bare = review_repo(data, &ws)?;
    if !bare.is_dir() {
        bail!("no review copy of thread {thread_id}'s commits");
    }
    std::fs::create_dir_all(dir.parent().context("check copy parent")?)?;
    // The checkout writes files the building agent chose onto this PC, so
    // nothing in them may make git do more: no hooks, no filters (Git for
    // Windows turns on LFS, whose .lfsconfig could point anywhere), and none of
    // the PC's own git settings.
    let null = null_device();
    let hooks = no_hooks();
    let safe = [
        "-c",
        &hooks,
        "-c",
        "core.fsmonitor=false",
        "-c",
        "filter.lfs.smudge=",
        "-c",
        "filter.lfs.process=",
        "-c",
        "filter.lfs.required=false",
        "-c",
        "protocol.allow=never",
        "-c",
        "protocol.file.allow=always",
    ];
    let to = dir.to_string_lossy().into_owned();
    let from = bare.to_string_lossy().into_owned();
    let mut args: Vec<&str> = safe.to_vec();
    args.extend([
        "clone",
        "--quiet",
        "--no-hardlinks",
        "--branch",
        branch,
        "--",
        &from,
        &to,
    ]);
    git_isolated(Path::new("."), &args, null).await?;
    // The last git command run here: the agent has not started yet.
    let mut args: Vec<&str> = safe.to_vec();
    args.extend(["branch", "decurion-base", base]);
    git_isolated(dir, &args, null).await?;
    Ok(())
}

/// Runs one coordinator, design or review job. Never posts events: what the
/// helper says only matters as its final answer, which the hub checks.
async fn run_helper(
    hub: &Hub,
    data: &Path,
    gate: &Gate,
    job: &HelperClaim,
) -> anyhow::Result<HelperResult> {
    let name = helper_container_name(job.job_id);
    let hb = HelperBox::for_job(data, job);
    // A job handed out again after a runner restart starts from scratch.
    let _ = docker_out(&["rm", "-f", &name]).await;
    let _ = docker_out(&["rm", "-f", &helper_tool_container_name(job.job_id)]).await;
    hb.clean().await;
    let net = BoxNet::helper(job.job_id);
    let relay = gate.relay(if job.budget_usd.is_some() {
        Credential::PayPerUse
    } else {
        Credential::Max(job.app.profile)
    });
    let r = match start_site_gate(hub, net.clone(), &job.app, None, relay.as_deref()).await {
        Ok(exit) => {
            let r = helper_container(hub, data, gate, job, &name, &net, &hb).await;
            // `--rm` only applies if the docker client lived to see the exit,
            // and the network can only go once the box has.
            let _ = docker_out(&["rm", "-f", &name]).await;
            exit.stop().await;
            r
        }
        Err(e) => Err(e),
    };
    hb.clean().await;
    r
}

/// The longest a helper job may run before it is stopped.
const HELPER_TIME_LIMIT: Duration = Duration::from_secs(60 * 60);

/// A failed helper job with nothing charged from Claude's own report.
fn helper_failed(text: String) -> HelperResult {
    HelperResult {
        ok: false,
        text,
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        tokens: None,
        page_html: None,
    }
}

async fn helper_container(
    hub: &Hub,
    data: &Path,
    gate: &Gate,
    job: &HelperClaim,
    name: &str,
    net: &BoxNet,
    hb: &HelperBox,
) -> anyhow::Result<HelperResult> {
    match &job.source {
        // Made once per goal and kept for its later turns, rebuilt when a
        // later round of the goal moved its start commit.
        HelperSource::App {
            goal_id,
            start_commit: Some(start),
        } => copy_at_start(hub, data, job.job_id, *goal_id, start, &hb.dir, true).await?,
        // A fresh copy for each design job.
        HelperSource::AppCopy {
            goal_id,
            start_commit: Some(start),
        } => copy_at_start(hub, data, job.job_id, *goal_id, start, &hb.dir, false).await?,
        // Goals from before start commits copy the folder, as they did.
        HelperSource::App {
            start_commit: None, ..
        } => goal_copy(&job.app, &hb.dir).await?,
        HelperSource::AppCopy {
            start_commit: None, ..
        } => {
            std::fs::create_dir_all(hb.dir.parent().context("design copy parent")?)?;
            clone_into(&job.app.repo_path, &hb.dir).await?;
        }
        HelperSource::Branch {
            thread_id,
            branch,
            checkpoint,
        } => {
            if !valid_branch(branch) {
                bail!("refusing odd branch name {branch:?}");
            }
            if let Some(cp) = checkpoint {
                let xfer = data.join("transfer").join(format!("check-{thread_id}"));
                sync_review_copy(hub, data, &job.app, *thread_id, branch, *cp, &xfer).await?;
            }
            branch_copy(data, &job.app, *thread_id, branch, &hb.dir).await?
        }
    }
    if let Some(out) = &hb.out {
        std::fs::create_dir_all(out)?;
    }
    // Into the helper's own home, before it starts. The coordinator has no
    // shell, but it can still read these with its Read tool.
    let xfer = data.join("transfer").join(format!("helper-{}", job.job_id));
    remove_all(&xfer)?;
    std::fs::create_dir_all(&xfer)?;
    let put = put_context(
        hub,
        &job.context,
        &format!("/runner/helper/{}/context", job.job_id),
        &xfer,
        &helper_tool_container_name(job.job_id),
        &hb.home_volume,
    )
    .await;
    let _ = remove_all(&xfer);
    put?;
    let cred = if job.budget_usd.is_some() {
        Credential::PayPerUse
    } else {
        Credential::Max(job.app.profile)
    };
    // Kept until the container is gone; its token goes when it drops.
    let (grant, env) = gate.credential(cred, job.reserved_micros)?;
    let args = helper_run_args(
        &Sandbox {
            name,
            src: &hb.dir,
            read_only: hb.read_only,
            home_volume: &hb.home_volume,
            network: &net.network,
            out: hb.out.as_deref(),
        },
        &job.session_id,
        job.resume,
        cred,
        job.budget_usd,
        &job.disallowed_tools,
    )?;
    // As with work runs: from here it may cost money, so no start unless the
    // hub knows.
    hub.post(
        &format!("/runner/helper/{}/started", job.job_id),
        &serde_json::json!({}),
    )
    .await
    .context("could not tell the hub the job is starting")?;
    let mut child = docker()
        .envs(env)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("could not start docker")?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let mut prompt = job.prompt.clone();
    if let Some(more) = context_prompt(&job.context) {
        prompt.push_str("\n\n");
        prompt.push_str(&more);
    }
    if let Some(more) = patient_data_prompt(&job.app) {
        prompt.push_str("\n\n");
        prompt.push_str(more);
    }
    // Written from its own task, so stdout and stderr are read meanwhile: a
    // big prompt and a chatty container could otherwise each wait on the
    // other's full pipe.
    tokio::spawn(async move {
        if let Err(err) = stdin.write_all(prompt.as_bytes()).await {
            tracing::warn!(%err, "could not hand the helper its prompt");
        }
        // Dropping stdin closes it, which tells Claude the task is complete.
    });
    let stderr_task = spawn_stderr_tail(child.stderr.take().context("no stderr")?, Vec::new());
    let mut out = BufReader::new(child.stdout.take().context("no stdout")?);
    // Same trust rule as `run`, except that two results fail the job: a cost
    // only counts from Claude's one result in a container that exited cleanly.
    let ran = tokio::time::timeout(HELPER_TIME_LIMIT, async {
        let mut watch = FirstResult::default();
        let mut meter = ContextMeter::default();
        while let Some((line, _)) = read_capped_line(&mut out).await? {
            meter.see(&line);
            let parsed = parse_result(&line).map(|mut r| {
                meter.stamp(&mut r.tokens);
                r
            });
            watch.push(&line, parsed);
        }
        let status = child.wait().await?;
        anyhow::Ok((watch.helper_answer(), status))
    })
    .await;
    let mut r = match ran {
        Err(_) => {
            let _ = docker_out(&["rm", "-f", name]).await;
            let _ = child.kill().await;
            stderr_task.abort();
            helper_failed(format!(
                "The job took too long and was stopped after {} minutes.",
                HELPER_TIME_LIMIT.as_secs() / 60
            ))
        }
        Ok(ran) => {
            let (result, status) = ran?;
            let err = stderr_task.await.unwrap_or_default();
            match result {
                Some(mut r) => {
                    if !status.success() {
                        r.cost_usd = None;
                    }
                    if r.ok && !status.success() {
                        r.ok = false;
                        r.text = format!(
                            "container exited with {status} after the agent answered: {}",
                            r.text
                        );
                    }
                    r
                }
                None => helper_failed(format!(
                    "container ended without a result ({status}): {}",
                    err.trim()
                )),
            }
        }
    };
    // Make sure the container is gone, so the page cannot change while it is
    // read and the token is not revoked under a container still running.
    let _ = docker_out(&["rm", "-f", name]).await;
    // For pay-per-use, the gatekeeper's count is the cost, as in `run`: the
    // higher of it and Claude's own trusted report, and nothing if the count
    // cannot be trusted.
    let metered_usd = match &grant {
        Some(g) if g.finish(Duration::from_secs(30)).await => {
            Some((job.prior_micros.max(0) as u64 + g.spent_micros()) as f64 / 1_000_000.0)
        }
        _ => None,
    };
    let used_up = grant.as_ref().is_some_and(Grant::used_up);
    let pay_per_use = grant.is_some();
    drop(grant);
    if pay_per_use {
        r.cost_usd = metered_usd.map(|m| r.cost_usd.map_or(m, |c| m.max(c)));
        if !r.ok && used_up {
            r.text = BUDGET_STOP.to_owned();
        }
    }
    if let Some(dir) = &hb.out {
        r.page_html = read_page(dir);
    }
    Ok(r)
}

/// How long a helper job's end keeps being retried while the hub is down or
/// failing. Short enough that a hub stuck failing does not hold up every
/// goal's helpers for long.
const FINISH_RETRY_FOR: Duration = Duration::from_secs(10 * 60);

/// Reports a helper job's end, trying again for a while if the hub is down or
/// failing. If it never gets through, the hub hands the job out again once
/// this runner says hello after a restart.
async fn finish_helper(hub: &Hub, id: i64, mut r: HelperResult) {
    let path = format!("/runner/helper/{id}/finish");
    let give_up = Instant::now() + FINISH_RETRY_FOR;
    let mut wait = Duration::from_secs(2);
    loop {
        let Err(err) = hub.post(&path, &r).await else {
            return;
        };
        let status = hub_status(&err);
        // A page too big for the hub must not keep the job open.
        if status == Some(StatusCode::PAYLOAD_TOO_LARGE) && r.page_html.is_some() {
            tracing::warn!(id, "hub refused the page as too big; reporting without it");
            r.page_html = None;
            continue;
        }
        // A hub that answered and said no will say no again; one that is
        // down or failing may recover.
        let retry = match status {
            Some(s) => s.is_server_error(),
            None => matches!(&err, HubError::Unreachable(_, e) if !e.is_builder()),
        };
        if !retry || Instant::now() + wait > give_up {
            tracing::error!(%err, id, "could not report helper job");
            return;
        }
        tracing::warn!(%err, id, "could not report helper job; retrying");
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(Duration::from_secs(60));
    }
}

/// Takes coordinator, design and review jobs one at a time, alongside the
/// work loop, so a goal's planning never waits for a long build.
/// Opens the sign-in windows the owner asks for, one at a time: a fresh
/// browser the owner drives from the board or phone, whose sign-in is sealed
/// into this PC's vault when the owner taps "I'm signed in, save".
async fn sign_in_loop(hub: Hub, data: PathBuf) {
    let mut hub_down = false;
    loop {
        let Some(_job) = SideJob::start() else {
            return;
        };
        let r = hub
            .post("/runner/browser/claim", &serde_json::json!({}))
            .await;
        let session: SignInSession = match r {
            Ok(resp) if resp.status() == StatusCode::NO_CONTENT => {
                hub_down = false;
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
            Ok(resp) => match resp.json().await {
                Ok(s) => s,
                Err(err) => {
                    tracing::warn!(%err, "could not read a sign-in window from the hub");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }
            },
            Err(err) => {
                if !hub_down {
                    tracing::warn!(%err, "could not ask the hub for sign-in windows");
                }
                hub_down = true;
                tokio::time::sleep(Duration::from_secs(10)).await;
                continue;
            }
        };
        let sid = session.session_id;
        tracing::info!(
            sid,
            sign_in = session.sign_in_id,
            "opening a sign-in window"
        );
        match browser_box::run_sign_in_box(&hub, sid, &session.start_url).await {
            Ok((c, window)) => {
                let captured = FromBrowser::Captured {
                    state: c.state,
                    login_url: c.login_url,
                    domains: c.domains,
                    expires_at: c.expires_at,
                };
                // The window stays open on the owner's screen until this is
                // known, and the hub takes the save only while it is.
                let why =
                    match sign_ins::save_capture(&hub.access(), &data, &session, &captured).await {
                        Ok(()) => browser_box::SIGN_IN_SAVED.to_owned(),
                        Err(err) => {
                            tracing::error!(%err, sid, "could not save a sign-in");
                            "The sign-in could not be kept. Close this and try again.".to_owned()
                        }
                    };
                window.finish(&why).await;
            }
            Err(err) => tracing::info!(%err, sid, "sign-in window ended without a save"),
        }
    }
}

/// What asking the hub for a helper job came to.
enum HelperAsk {
    Job(Box<HelperClaim>),
    Nothing,
    /// The hub follows other rules now.
    OtherRules,
    Retry,
}

async fn ask_for_helper(hub: &Hub) -> HelperAsk {
    let r = hub
        .post(
            "/runner/helper/claim",
            &serde_json::json!({ "runner": hub.name, "rules_version": RULES_VERSION }),
        )
        .await;
    match r {
        Ok(resp) if resp.status() == StatusCode::NO_CONTENT => HelperAsk::Nothing,
        Ok(resp) => match resp.json().await {
            Ok(job) => HelperAsk::Job(Box::new(job)),
            Err(err) => {
                // The hub requeues it when this runner restarts.
                tracing::error!(%err, "could not read claimed helper job");
                HelperAsk::Retry
            }
        },
        // As for work claims: a 409 that names the hub's rules.
        Err(HubError::Refused(_, s, text))
            if s == StatusCode::CONFLICT && serde_json::from_str::<HelloReply>(&text).is_ok() =>
        {
            HelperAsk::OtherRules
        }
        Err(err) => {
            tracing::warn!(%err, "could not ask the hub for helper jobs; retrying");
            HelperAsk::Retry
        }
    }
}

/// True if the folder `dir` is a copy checked out, detached, at exactly
/// `sha`. Read from the file rather than by asking git.
fn copy_is_at(dir: &Path, sha: &str) -> bool {
    std::fs::read_to_string(dir.join(".git").join("HEAD")).is_ok_and(|h| h.trim() == sha)
}

async fn helper_loop(hub: Hub, data: PathBuf, gate: Gate) {
    loop {
        let Some(_job) = SideJob::start() else {
            return;
        };
        match ask_for_helper(&hub).await {
            HelperAsk::Nothing => tokio::time::sleep(Duration::from_secs(3)).await,
            HelperAsk::Retry => tokio::time::sleep(Duration::from_secs(5)).await,
            HelperAsk::OtherRules => {
                // The work loop's own next claim hears the same and updates
                // the runner once what is in hand is finished.
                tracing::warn!("the hub follows other rules now; taking no more helper jobs");
                WINDING_DOWN.store(true, std::sync::atomic::Ordering::SeqCst);
                return;
            }
            HelperAsk::Job(job) => {
                let job = *job;
                let id = job.job_id;
                tracing::info!(id, kind = job.kind.as_str(), "helper job");
                let result = match run_helper(&hub, &data, &gate, &job).await {
                    Ok(r) => r,
                    Err(err) => {
                        tracing::error!(%err, id, "helper job failed");
                        helper_failed(format!("runner error: {err:#}"))
                    }
                };
                finish_helper(&hub, id, result).await;
            }
        }
    }
}

/// Throwaway helper copies, homes and pages left by a runner that died.
/// Their jobs come back from the hub and start fresh.
async fn clean_helper_leftovers(data: &Path) {
    for d in ["checks", "designs", "out", "previews"] {
        remove_dir(&data.join(d));
    }
    if let Ok(out) = docker_out(&["volume", "ls", "-q"]).await {
        let vols: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .filter(|v| {
                v.starts_with("decurion-check-home-")
                    || v.starts_with("decurion-design-home-")
                    || v.starts_with("decurion-preview-home-")
            })
            .map(str::to_owned)
            .collect();
        if !vols.is_empty() {
            let _ = docker()
                .args(["volume", "rm", "-f"])
                .args(&vols)
                .output()
                .await;
        }
    }
}

/// A cloud worker's identity: read from `cloud-runner.json`, or on first
/// start swapped for the one-time join code. The code is never used again.
async fn cloud_identity(data: &Path, http: &reqwest::Client) -> anyhow::Result<CloudRunner> {
    let file = data.join("cloud-runner.json");
    match std::fs::read(&file) {
        Ok(b) => return CloudRunner::parse(&b),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(e).context("could not read cloud-runner.json")
        }
        Err(_) => {}
    }
    let code = std::env::var("DECURION_JOIN_CODE")
        .ok()
        .filter(|c| !c.trim().is_empty())
        .context("first start of a cloud worker needs DECURION_JOIN_CODE from the board's Add cloud worker page")?;
    let hub = hub_url(
        &std::env::var("DECURION_HUB")
            .context("first start of a cloud worker needs DECURION_HUB")?,
    )?;
    let mut attempt = 0;
    loop {
        let r = http
            .post(format!("{hub}/runner/join"))
            .timeout(CALL_TIMEOUT)
            .json(&JoinRequest {
                code: code.trim().to_owned(),
            })
            .send()
            .await;
        match r {
            Ok(r) if r.status().is_success() => {
                let j: JoinReply = r.json().await.context("could not read the join reply")?;
                let me = CloudRunner {
                    hub,
                    name: j.name,
                    token: j.token,
                };
                write_atomic(&file, &serde_json::to_vec_pretty(&me)?, true)?;
                tracing::info!(name = %me.name, "joined the hub");
                return Ok(me);
            }
            Ok(r) if r.status() == StatusCode::UNAUTHORIZED => bail!(
                "the hub refused the join code: it was used already or is more than a day old. \
                 Make a new one on the board's Add cloud worker page"
            ),
            Ok(r) if !retriable(Some(r.status().as_u16())) => {
                bail!("could not join the hub: {}", r.status())
            }
            Ok(r) => tracing::warn!(status = %r.status(), "could not join the hub yet"),
            Err(err) => tracing::warn!(%err, "hub unreachable; will try joining again"),
        }
        tokio::time::sleep(backoff(attempt)).await;
        attempt += 1;
    }
}

/// Says hello, which also requeues work this runner had in flight. Keeps
/// trying while the hub is out of reach. If the hub follows other rules,
/// records the commit the hub was built from and exits with code 3, so the
/// cloud server's updater rebuilds the runner at that commit.
async fn hello(hub: &Hub, data: &Path) -> anyhow::Result<HelloReply> {
    let body = RunnerHello {
        runner: hub.name.clone(),
        rules_version: RULES_VERSION,
    };
    let mut attempt = 0;
    loop {
        let r = hub
            .http
            .post(hub.url("/runner/hello"))
            .bearer_auth(&hub.key)
            .timeout(CALL_TIMEOUT)
            .json(&body)
            .send()
            .await;
        match r {
            Ok(r) if r.status().is_success() => {
                let reply: HelloReply = r.json().await.context("could not read hello reply")?;
                return Ok(reply);
            }
            Ok(r) if r.status() == StatusCode::CONFLICT => {
                let reply: HelloReply = r
                    .json()
                    .await
                    .context("hub refused this runner and gave no reason")?;
                match rules_differ(data, &reply)? {}
            }
            Ok(r) if !retriable(Some(r.status().as_u16())) => bail!(
                "hub refused this runner: {} {}",
                r.status(),
                r.text().await.unwrap_or_default()
            ),
            Ok(r) => tracing::warn!(status = %r.status(), "hub not ready; retrying"),
            Err(err) => tracing::warn!(%err, "hub unreachable; retrying"),
        }
        tokio::time::sleep(backoff(attempt)).await;
        attempt += 1;
    }
}

/// The hub follows other rules: records the commit it was built from for the
/// cloud server's updater and exits with code 3, so the runner is rebuilt at
/// that commit and started again.
fn rules_differ(data: &Path, reply: &HelloReply) -> anyhow::Result<std::convert::Infallible> {
    let commit = reply.hub_commit.trim();
    if is_sha(commit) {
        write_atomic(
            &data.join("wanted-commit"),
            format!("{commit}\n").as_bytes(),
            false,
        )?;
    }
    tracing::error!(
        runner_rules = RULES_VERSION,
        hub_rules = reply.rules_version,
        hub_commit = commit,
        "this runner follows different rules from the hub; rebuild it at the hub's commit and start it again"
    );
    std::process::exit(3);
}

/// On Linux the agent's files in the work folder belong to the agent's user,
/// and the runner must be able to read, save and delete them; the agent must
/// be able to write what the runner unpacked. That only works if both are the
/// same user (or the runner is root), so refuse to start otherwise rather than
/// fail on the first piece of work. Docker Desktop on Windows has no such
/// problem.
#[cfg(unix)]
async fn same_user_as_agent(mine: &std::fs::File) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let me = mine.metadata()?.uid();
    if me == 0 {
        return Ok(());
    }
    let out = docker()
        .args([
            "run",
            "--rm",
            "--network",
            "none",
            "--entrypoint",
            "id",
            decurion_runner::IMAGE,
            "-u",
        ])
        .output()
        .await
        .context("could not start docker")?;
    let agent = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !out.status.success() || agent != me.to_string() {
        bail!(
            "the agent image's user ({agent}) is not this runner's user ({me}); rebuild it with \
             `docker build --build-arg AGENT_UID={me} -t {} docker/claude-worker`",
            decurion_runner::IMAGE
        );
    }
    Ok(())
}

/// Port a cloud worker's gatekeeper listens on: the cloud walls open exactly
/// this port to containers. Elsewhere any free port will do.
const CLOUD_GATE_PORT: u16 = 7421;

/// Starts the gatekeeper where only this computer's containers can reach it.
async fn start_gate(place: Place) -> anyhow::Result<Gate> {
    // Docker Desktop (Windows, Mac) forwards host.docker.internal to the
    // host's loopback, so nothing off this computer can reach it. On Linux,
    // containers reach the host at the Docker bridge's own address (a box's
    // gate maps host.docker.internal there), which only containers and the
    // host itself can use.
    let addr = if cfg!(target_os = "linux") {
        let port = if place == Place::Cloud {
            CLOUD_GATE_PORT
        } else {
            0
        };
        std::net::SocketAddr::new(wait_for_bridge().await?, port)
    } else {
        std::net::SocketAddr::from(([127, 0, 0, 1], 0))
    };
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("could not start the gatekeeper on {addr}"))?;
    let gate = Gate {
        keeper: Gatekeeper::new(ANTHROPIC_API)?,
        url: format!(
            "http://host.docker.internal:{}",
            listener.local_addr()?.port()
        ),
    };
    let serving = gate.keeper.clone();
    tokio::spawn(async move {
        if let Err(err) = serving.serve(listener).await {
            tracing::error!(%err, "gatekeeper stopped");
        }
    });
    Ok(gate)
}

/// How long the runner waits for Docker to come up at start.
const DOCKER_WAIT: Duration = Duration::from_secs(5 * 60);

/// The Docker bridge's address, waiting a while for Docker: at boot the
/// runner can start before Docker is ready.
async fn wait_for_bridge() -> anyhow::Result<std::net::IpAddr> {
    let start = Instant::now();
    let mut attempt = 0;
    loop {
        let err = match bridge_gateway().await {
            Ok(ip) => return Ok(ip),
            Err(e) => e,
        };
        let wait = backoff(attempt);
        if start.elapsed() + wait > DOCKER_WAIT {
            return Err(err);
        }
        tracing::warn!(%err, wait_secs = wait.as_secs(), "waiting for Docker to start");
        tokio::time::sleep(wait).await;
        attempt += 1;
    }
}

/// The host's address on Docker's default bridge, which is what
/// `host-gateway` resolves to inside containers.
async fn bridge_gateway() -> anyhow::Result<std::net::IpAddr> {
    let out = docker()
        .args([
            "network",
            "inspect",
            "bridge",
            "--format",
            "{{range .IPAM.Config}}{{.Gateway}} {{end}}",
        ])
        .output()
        .await
        .context("could not start docker")?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .find_map(|s| s.parse::<std::net::Ipv4Addr>().ok())
        .map(std::net::IpAddr::V4)
        .context("could not find the Docker bridge's address; is Docker running?")
}

/// A box's site gate: its network, the proxy container on it, and the task
/// passing what the gate blocks on to the board.
struct SiteGate {
    net: BoxNet,
    child: tokio::process::Child,
    reader: tokio::task::JoinHandle<()>,
}

/// Removes a box's gate and network, if a runner that died left them.
async fn clear_gate(net: &BoxNet) {
    let _ = docker().args(["rm", "-f", &net.gate]).output().await;
    let _ = docker()
        .args(["network", "rm", &net.network])
        .output()
        .await;
}

/// Makes the box's network and starts its gate, letting through only the
/// built-in sites and the app's own list. Returns once the gate is
/// listening and connected to the outside. Blocked visits are posted to
/// `thread`'s activity when there is one; helper jobs only log them.
async fn start_site_gate(
    hub: &Hub,
    net: BoxNet,
    app: &App,
    thread: Option<i64>,
    relay: Option<&str>,
) -> anyhow::Result<SiteGate> {
    clear_gate(&net).await;
    match gate_up(hub, &net, app, thread, relay).await {
        Ok((child, reader)) => Ok(SiteGate { net, child, reader }),
        Err(e) => {
            clear_gate(&net).await;
            Err(e)
        }
    }
}

async fn gate_up(
    hub: &Hub,
    net: &BoxNet,
    app: &App,
    thread: Option<i64>,
    relay: Option<&str>,
) -> anyhow::Result<(tokio::process::Child, tokio::task::JoinHandle<()>)> {
    let out = docker()
        .args([
            "network",
            "create",
            "--internal",
            // Without this the PC itself takes an address on the network,
            // and anything listening on the PC is in reach of the box.
            "-o",
            "com.docker.network.bridge.inhibit_ipv4=true",
            &net.network,
        ])
        .output()
        .await?;
    if !out.status.success() {
        bail!(
            "could not make the box's network: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let mut child = docker()
        .args(gate_args(net, &sites::for_app(&app.sites), relay))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("could not start the gate")?;
    let stderr = child.stderr.take().context("no stderr")?;
    let err_task = tokio::spawn(async move {
        let mut tail = String::new();
        let mut r = BufReader::new(stderr);
        while let Ok(Some((line, _))) = read_capped_line(&mut r).await {
            if tail.len() < STDERR_TAIL {
                tail.push_str(&line);
                tail.push('\n');
            }
        }
        tail
    });
    let mut out = BufReader::new(child.stdout.take().context("no stdout")?);
    let ready = tokio::time::timeout(Duration::from_secs(60), async {
        while let Some((line, _)) = read_capped_line(&mut out).await? {
            if parse_gate_line(&line) == Some(GateLine::Ready) {
                return Ok(true);
            }
        }
        std::io::Result::Ok(false)
    })
    .await;
    if !matches!(ready, Ok(Ok(true))) {
        let _ = child.kill().await;
        let err = err_task.await.unwrap_or_default();
        bail!(
            "the box's gate did not start: {}. If the image is missing, build it with \
             `docker build -t {GATE_IMAGE} -f docker/gate/Dockerfile .`",
            err.trim()
        );
    }
    // Only now does the gate get a way out; the box itself never does.
    let out2 = docker()
        .args(["network", "connect", "bridge", &net.gate])
        .output()
        .await?;
    if !out2.status.success() {
        bail!(
            "could not connect the gate to the internet: {}",
            String::from_utf8_lossy(&out2.stderr).trim()
        );
    }
    let hub = hub.clone();
    let reader = tokio::spawn(async move {
        while let Ok(Some((line, _))) = read_capped_line(&mut out).await {
            if let Some(GateLine::Blocked { host, reason }) = parse_gate_line(&line) {
                match thread {
                    Some(id) => hub.event(id, &RunEvent::Blocked { host, reason }).await,
                    None => tracing::info!(%host, %reason, "helper box was blocked"),
                }
            }
        }
    });
    Ok((child, reader))
}

impl SiteGate {
    async fn stop(mut self) {
        let _ = docker().args(["rm", "-f", &self.net.gate]).output().await;
        let _ = self.child.wait().await;
        let _ = self.reader.await;
        // The box is removed as it exits, but give Docker a moment to let go
        // of the network.
        for _ in 0..5 {
            let ok = docker()
                .args(["network", "rm", &self.net.network])
                .output()
                .await
                .is_ok_and(|o| o.status.success());
            if ok {
                return;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        tracing::warn!(network = %self.net.network, "could not remove the box's network");
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "decurion_runner=info".into()),
        )
        .init();
    let data = data_dir()?;
    std::fs::create_dir_all(&data)?;
    // One runner per hub: a second one would requeue and kill the first one's
    // work. The OS releases the lock if this process dies.
    let lock = std::fs::File::create(data.join("runner.lock"))?;
    if lock.try_lock().is_err() {
        bail!("another Decurion runner is already running on this computer");
    }
    let place = place_from_env(std::env::var("DECURION_PLACE").ok().as_deref())?;
    let gate = start_gate(place).await?;
    // A sleeping hub must not hang a call forever: give up on dead
    // connections, then retry.
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .tcp_keepalive(Duration::from_secs(30))
        .tcp_keepalive_interval(Duration::from_secs(15))
        .build()?;
    let hub = match place {
        Place::Pc => {
            let keys: serde_json::Value = serde_json::from_slice(
                &std::fs::read(data.join("keys.json"))
                    .context("start the hub first; it creates keys.json")?,
            )?;
            Hub {
                base: hub_url(&std::env::var("DECURION_HUB").unwrap_or_else(|_| PC_HUB.into()))?,
                key: keys["runner"]
                    .as_str()
                    .context("keys.json has no runner key")?
                    .to_owned(),
                http,
                name: PC_NAME.to_owned(),
                place,
            }
        }
        Place::Cloud => {
            let me = cloud_identity(&data, &http).await?;
            Hub {
                base: me.hub,
                key: me.token,
                http,
                name: me.name,
                place,
            }
        }
    };
    tracing::info!(name = %hub.name, place = place.as_str(), hub = %hub.base, "runner starting");
    #[cfg(unix)]
    same_user_as_agent(&lock).await?;
    // Containers left by a runner that died keep working, and spending, with
    // nobody listening. Stop them all before taking on any work.
    if let Ok(out) = docker()
        .args([
            "ps",
            "-aq",
            "--filter",
            "name=^decurion-thread-",
            "--filter",
            "name=^decurion-helper-",
            // Browser boxes: a run's (decurion-thread-{id}-browser, caught
            // above) and a sign-in window's.
            "--filter",
            "name=^decurion-signin-",
            // A preview's boxes: the app's and its browser.
            "--filter",
            "name=^decurion-preview-",
            "--filter",
            "name=^decurion-gate-",
        ])
        .output()
        .await
    {
        let ids: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if !ids.is_empty() {
            tracing::info!(
                count = ids.len(),
                "stopping containers left by a previous runner"
            );
            let _ = docker().arg("rm").arg("-f").args(&ids).output().await;
        }
    }
    // Then the boxes' own networks, now that nothing is attached to them.
    if let Ok(out) = docker()
        .args(["network", "ls", "-q", "--filter", "name=^decurion-net-"])
        .output()
        .await
    {
        let ids: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if !ids.is_empty() {
            let _ = docker().arg("network").arg("rm").args(&ids).output().await;
        }
    }
    clean_helper_leftovers(&data).await;
    // Checked before saying hello: hello puts this runner's running work
    // back in the queue, so a bad setting must stop it first.
    let wanted = dispatch::max_runs(place, std::env::var("DECURION_MAX_RUNS").ok().as_deref())
        .map_err(anyhow::Error::msg)?;
    let reply = hello(&hub, &data).await?;
    if !reply.paused.is_empty() {
        tracing::info!(
            paused = ?reply.paused,
            "requeued work this runner had in flight; it will resume"
        );
    }
    // An older hub ignores the busy list, so two attempts at one piece could
    // overlap here; with it, run one at a time until the hub is updated.
    let max = if reply.concurrent { wanted } else { 1 };
    if max < wanted {
        tracing::warn!(
            wanted,
            "this hub is older and hands out one piece at a time; update it to run several"
        );
    }
    tracing::info!(max_runs = max, "taking work");
    // Coordinator, design and review jobs work on the app's own repo, which
    // only the PC has.
    if place == Place::Pc {
        tokio::spawn(helper_loop(hub.clone(), data.clone(), gate.clone()));
        // Sign-in windows save into this PC's vault, so only the PC opens them.
        tokio::spawn(sign_in_loop(hub.clone(), data.clone()));
        // Previews run on the PC, with its test keys and the app's own folder.
        tokio::spawn(preview_box::preview_loop(hub.clone(), data.clone()));
    }
    let drained: std::cell::RefCell<Option<HelloReply>> = std::cell::RefCell::new(None);
    let (h, d, g, dr) = (&hub, data.as_path(), &gate, &drained);
    dispatch::dispatch(
        max,
        move |busy| claim_next(h, busy, dr),
        move |claim| run_one(h, d, g, claim),
    )
    .await;
    // Only a drain ends the loop: the hub was updated to other rules, and
    // every running piece has now finished.
    let reply = drained
        .take()
        .context("stopped taking work for no reason")?;
    // Let a helper job or sign-in window already under way finish too.
    while SIDE_JOBS.load(std::sync::atomic::Ordering::SeqCst) > 0 {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    match rules_differ(&data, &reply)? {}
}

/// Set once the hub has moved to other rules: no new helper jobs or sign-in
/// windows are taken either, only what is in hand is finished.
static WINDING_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Helper jobs and sign-in windows being asked for or worked on now. Counted
/// from before the ask, so none can slip in while the runner checks it.
static SIDE_JOBS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Counts one side job for as long as it lives.
struct SideJob;

impl SideJob {
    /// None once winding down: take nothing new.
    fn start() -> Option<Self> {
        use std::sync::atomic::Ordering;
        SIDE_JOBS.fetch_add(1, Ordering::SeqCst);
        if WINDING_DOWN.load(Ordering::SeqCst) {
            SIDE_JOBS.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(SideJob)
    }
}

impl Drop for SideJob {
    fn drop(&mut self) {
        SIDE_JOBS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Asks the hub for one more piece of work, listing the ones still busy
/// here. Waits a little before saying there is none, so the dispatcher does
/// not ask again at once; runs keep going meanwhile.
async fn claim_next(
    hub: &Hub,
    busy: Vec<i64>,
    drained: &std::cell::RefCell<Option<HelloReply>>,
) -> dispatch::Next<Claim> {
    let r = hub
        .post(
            "/runner/claim",
            &serde_json::json!({
                "runner": hub.name,
                "rules_version": RULES_VERSION,
                "busy": busy,
            }),
        )
        .await;
    match r {
        Ok(resp) if resp.status() == StatusCode::NO_CONTENT => {
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        Ok(resp) => match resp.json::<Claim>().await {
            Ok(claim) => return dispatch::Next::Work(claim.thread.id, claim),
            Err(err) => {
                // The hub already marked it running; a runner restart will
                // requeue it.
                tracing::error!(%err, "could not read claimed work");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        },
        // The hub was updated while this runner was working.
        Err(HubError::Refused(_, s, text)) if s == StatusCode::CONFLICT => {
            match serde_json::from_str::<HelloReply>(&text) {
                Ok(reply) => {
                    tracing::warn!(
                        "the hub follows other rules now; finishing the work in hand first"
                    );
                    *drained.borrow_mut() = Some(reply);
                    WINDING_DOWN.store(true, std::sync::atomic::Ordering::SeqCst);
                    return dispatch::Next::Drain;
                }
                Err(_) => {
                    tracing::warn!(%text, "hub refused to hand out work; retrying");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
        Err(err) => {
            tracing::warn!(%err, "could not ask the hub for work; retrying");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
    dispatch::Next::Idle
}

/// Runs one claimed piece of work to the very end: the run itself, then any
/// browser removal still going and a sweep for anything of this piece left
/// on this computer, so a later attempt at it starts clean. The piece stays
/// busy (the hub will not hand it back here) until this returns.
async fn run_one(hub: &Hub, data: &Path, gate: &Gate, claim: Claim) {
    let id = claim.thread.id;
    run_claimed(hub, data, gate, claim).await;
    browser_box::settle(id).await;
    addon_box::clear(data, id).await;
    if !remove_container(id).await {
        // Only logged: the run is over, so the board no longer takes this
        // piece's activity. The next attempt at it refuses to start while the
        // box is still there, and says so on the card.
        tracing::error!(
            id,
            container = %container_name(id),
            "could not remove the agent's container after its run"
        );
    }
    clear_gate(&BoxNet::work(id)).await;
}

async fn run_claimed(hub: &Hub, data: &Path, gate: &Gate, claim: Claim) {
    let id = claim.thread.id;
    tracing::info!(
        id,
        resume = claim.resume,
        checkpoint = ?claim.checkpoint,
        "running"
    );
    // Read here, not in `run`, so its errors can be hidden too.
    let keys = match test_keys(hub, data, &claim) {
        Ok(k) => k,
        Err(err) => {
            tracing::error!(%err, id, "could not read test keys");
            fail(hub, id, &err.to_string()).await;
            return;
        }
    };
    let mut secrets: Vec<String> = keys.iter().map(|(_, v)| v.clone()).collect();
    // The app's saved website sign-ins, for the run's browser box.
    // pi runs have no browser, so they get none.
    let (states, site_secrets) = if claim.thread.ai == decurion_proto::Ai::Claude {
        match sign_ins::site_states(data, hub.place, &claim) {
            Ok(s) => s,
            Err(err) => {
                tracing::error!(%err, id, "could not open saved sign-ins");
                fail(hub, id, &err.to_string()).await;
                return;
            }
        }
    } else {
        (Vec::new(), Vec::new())
    };
    // One that could not be opened needs the owner to sign in again.
    for r in sign_ins::missing(&claim, &states) {
        if let Err(err) = sign_ins::report_signed_out(&hub.access(), r.id, id, r.version).await {
            tracing::warn!(%err, id, sign_in = r.id, "could not report a missing sign-in");
        }
    }
    secrets.extend(site_secrets.iter().cloned());
    if let Err(err) = run(hub, data, gate, claim, &keys, states, site_secrets).await {
        let note = hide(&err.to_string(), &secrets);
        tracing::error!(err = %note, id, "run failed");
        fail(hub, id, &note).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn commit(dir: &Path, author: &str, msg: &str) -> anyhow::Result<String> {
        git(
            dir,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "--allow-empty",
                "--author",
                author,
                "-m",
                msg,
            ],
        )
        .await
    }

    /// A fake hub whose events route answers with `answers` in turn (the
    /// last one repeats), and how many times it was called.
    async fn fake_hub(
        answers: Vec<(u16, &'static str)>,
    ) -> (Hub, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let app = axum::Router::new().route(
            "/api/threads/{id}/events",
            axum::routing::post(move || {
                let n = seen.fetch_add(1, Ordering::SeqCst);
                let (code, body) = answers[n.min(answers.len() - 1)];
                async move { (StatusCode::from_u16(code).expect("status"), body) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await });
        let hub = Hub {
            base: format!("http://{addr}"),
            key: "k".into(),
            http: reqwest::Client::new(),
            name: "local".into(),
            place: Place::Pc,
        };
        (hub, calls)
    }

    /// A line the hub said it could not save (its database failed) is sent
    /// once more rather than lost. Any other failure, including a 500 from
    /// something in front of the hub, is not: the line may already be saved.
    #[tokio::test]
    async fn an_activity_line_is_sent_again_after_a_server_error() {
        use std::sync::atomic::Ordering;
        let line = RunEvent::Message {
            text: "still here".into(),
        };
        let (hub, calls) = fake_hub(vec![(500, HUB_DB_ERROR), (201, "1")]).await;
        hub.event(7, &line).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let (hub, calls) = fake_hub(vec![(500, HUB_DB_ERROR)]).await;
        hub.event(7, &line).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2, "only one retry");

        for other in [(500, "bad gateway"), (502, ""), (409, "")] {
            let (hub, calls) = fake_hub(vec![other]).await;
            hub.event(7, &line).await;
            assert_eq!(calls.load(Ordering::SeqCst), 1, "{other:?} is not retried");
        }
    }

    /// A goal's project lead and designer read exactly the goal's start
    /// commit, from the hub's bundle of it, in a copy of their own.
    #[tokio::test]
    async fn a_helper_copy_is_built_from_the_bundle_at_exactly_the_start() -> anyhow::Result<()> {
        let root =
            std::env::temp_dir().join(format!("decurion-helper-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(&src)?;
        git(&src, &["init", "--quiet"]).await?;
        std::fs::write(src.join("README.md"), "a")?;
        git(&src, &["add", "."]).await?;
        commit(&src, "t <t@example.invalid>", "a").await?;
        let a = git(&src, &["rev-parse", "HEAD"]).await?;
        commit(&src, "t <t@example.invalid>", "b").await?;
        let b = git(&src, &["rev-parse", "HEAD"]).await?;
        // The bundle the hub makes: one ref, decurion-base, at the start.
        git(&src, &["update-ref", "refs/heads/decurion-base", &a]).await?;
        let bundle = root.join("base.bundle");
        let bundle_s = bundle.to_string_lossy().into_owned();
        git(
            &src,
            &[
                "bundle",
                "create",
                "--quiet",
                &bundle_s,
                "refs/heads/decurion-base",
            ],
        )
        .await?;

        // An app whose name has a dot: its unfinished copy must not land
        // on another app's folder (`shop.web` cut to `shop.partial`).
        let other = root.join("goal").join("shop.partial");
        std::fs::create_dir_all(&other)?;
        std::fs::write(other.join("keep.txt"), "another app's")?;
        let dir = root.join("goal").join("shop.web-1");
        copy_from_bundle(&bundle, Some(&a), &a, &dir).await?;
        assert!(
            other.join("keep.txt").is_file(),
            "another folder was touched"
        );
        assert!(copy_is_at(&dir, &a));
        assert!(!copy_is_at(&dir, &b));
        assert!(!copy_is_at(&root.join("nowhere"), &a));
        assert_eq!(
            std::fs::read_to_string(dir.join(".git").join("HEAD"))?.trim(),
            a
        );
        assert_eq!(std::fs::read_to_string(dir.join("README.md"))?, "a");
        assert!(!dir
            .join(".git")
            .join("objects")
            .join("info")
            .join("alternates")
            .exists());
        assert_eq!(git(&dir, &["remote"]).await?, "");
        assert!(!sibling(&dir, "partial").exists());

        // Nothing is made from a start that is odd, unnamed, another one, or gone.
        for (n, (sent, want, from)) in [
            (Some("xyz"), "xyz", bundle.clone()),
            (None, a.as_str(), bundle.clone()),
            (Some(b.as_str()), a.as_str(), bundle.clone()),
            (Some(a.as_str()), a.as_str(), root.join("missing.bundle")),
        ]
        .into_iter()
        .enumerate()
        {
            let dir = root.join(format!("bad-{n}"));
            assert!(
                copy_from_bundle(&from, sent, want, &dir).await.is_err(),
                "case {n}"
            );
            assert!(!dir.exists(), "case {n}");
        }
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    /// A design job's copy asks the hub for the exact start it was claimed
    /// with, and is made even where an earlier try of the same job left a
    /// half-made copy behind.
    #[tokio::test]
    async fn a_helper_copy_asks_for_its_start_and_replaces_a_leftover() -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!("decurion-at-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(&src)?;
        git(&src, &["init", "--quiet"]).await?;
        std::fs::write(src.join("README.md"), "a")?;
        git(&src, &["add", "."]).await?;
        commit(&src, "t <t@example.invalid>", "a").await?;
        let a = git(&src, &["rev-parse", "HEAD"]).await?;
        git(&src, &["update-ref", "refs/heads/decurion-base", &a]).await?;
        let bundle = root.join("base.bundle");
        let bundle_s = bundle.to_string_lossy().into_owned();
        git(
            &src,
            &[
                "bundle",
                "create",
                "--quiet",
                &bundle_s,
                "refs/heads/decurion-base",
            ],
        )
        .await?;

        // A hub that serves the bundle and notes which start was asked for.
        let asked = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let seen = asked.clone();
        let bytes = std::fs::read(&bundle)?;
        let sha = a.clone();
        let app = axum::Router::new().route(
            "/api/runner/goals/{id}/base",
            axum::routing::get(
                move |axum::extract::Query(q): axum::extract::Query<
                    std::collections::HashMap<String, String>,
                >| {
                    *seen.lock().expect("lock") = q.get("at").cloned();
                    let (bytes, sha) = (bytes.clone(), sha.clone());
                    async move { ([("x-decurion-base", sha)], bytes) }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await });
        let hub = Hub {
            base: format!("http://{addr}"),
            key: "k".into(),
            http: reqwest::Client::new(),
            name: "local".into(),
            place: Place::Pc,
        };

        let dir = root.join("designs").join("shop-5-job9");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("half.txt"), "left by an earlier try")?;
        copy_at_start(&hub, &root.join("data"), 9, 5, &a, &dir, false).await?;
        assert_eq!(asked.lock().expect("lock").as_deref(), Some(a.as_str()));
        assert!(copy_is_at(&dir, &a));
        assert!(!dir.join("half.txt").exists());
        // An odd start is never sent to the hub.
        assert!(
            copy_at_start(&hub, &root.join("data"), 9, 5, "x&y", &dir, false)
                .await
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    /// The project lead's legacy copy of an app named like `shop.web` never
    /// lands on, or clears, another app's unfinished copy.
    #[tokio::test]
    async fn a_goal_copy_of_a_dotted_app_name_leaves_other_copies_alone() -> anyhow::Result<()> {
        let root = std::env::temp_dir().join(format!("decurion-goal-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(&src)?;
        git(&src, &["init", "--quiet"]).await?;
        commit(&src, "t <t@example.invalid>", "a").await?;
        let app: App = serde_json::from_value(serde_json::json!({
            "id": 1,
            "name": "shop.web",
            "repo_path": src.to_string_lossy(),
            "profile": "A",
        }))?;
        let other = root.join("goals").join("shop.partial");
        std::fs::create_dir_all(&other)?;
        std::fs::write(other.join("keep.txt"), "another app's")?;
        let dir = root.join("goals").join("shop.web-1");
        goal_copy(&app, &dir).await?;
        assert!(dir.join(".git").is_dir());
        assert!(
            other.join("keep.txt").is_file(),
            "another folder was touched"
        );
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    /// A fake hub whose helper-claim route answers `code` with `body`.
    async fn fake_helper_hub(code: u16, body: String) -> Hub {
        let app = axum::Router::new().route(
            "/api/runner/helper/claim",
            axum::routing::post(move || {
                let body = body.clone();
                async move { (StatusCode::from_u16(code).expect("status"), body) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await });
        Hub {
            base: format!("http://{addr}"),
            key: "k".into(),
            http: reqwest::Client::new(),
            name: "local".into(),
            place: Place::Pc,
        }
    }

    /// A hub on other rules turns the helper loop down the way it turns
    /// work claims down: the runner winds down to update, and does not
    /// keep asking as if the hub were unreachable.
    #[tokio::test]
    async fn a_hub_on_other_rules_winds_the_helper_loop_down() {
        let reply = HelloReply {
            paused: vec![],
            rules_version: RULES_VERSION + 1,
            hub_commit: "abc".into(),
            concurrent: true,
        };
        let hub = fake_helper_hub(409, serde_json::to_string(&reply).expect("json")).await;
        assert!(matches!(ask_for_helper(&hub).await, HelperAsk::OtherRules));
        let hub = fake_helper_hub(204, String::new()).await;
        assert!(matches!(ask_for_helper(&hub).await, HelperAsk::Nothing));
        let hub = fake_helper_hub(409, "busy".into()).await;
        assert!(matches!(ask_for_helper(&hub).await, HelperAsk::Retry));
    }

    /// An app holding a file whose full path is past Windows' old
    /// 260-character limit still copies, both through [`git`] (goal and work
    /// copies) and [`git_isolated`] (check copies). Before, Windows failed the
    /// checkout with "Filename too long" and the goal could not start.
    #[tokio::test]
    async fn apps_with_very_long_file_names_can_be_copied() -> anyhow::Result<()> {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("git not found; skipping");
            return Ok(());
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let root =
            std::env::temp_dir().join(format!("decurion-long-{}-{nanos}", std::process::id()));
        let src = root.join("app");
        let mut rel = PathBuf::new();
        for _ in 0..7 {
            rel.push("a-folder-name-that-is-about-forty-chars-long");
        }
        rel.push("notes.md");
        assert!(rel.to_string_lossy().len() > 300);
        std::fs::create_dir_all(src.join(&rel).parent().expect("has a folder"))?;
        std::fs::write(src.join(&rel), "notes\n")?;
        let r = async {
            git(&src, &["init", "--quiet"]).await?;
            git(&src, &["add", "."]).await?;
            commit(&src, "t <t@example.invalid>", "deep").await?;
            let listed = git(&src, &["ls-files"]).await?;
            anyhow::ensure!(listed.ends_with("notes.md"), "not committed: {listed:?}");
            let from = src.to_string_lossy().into_owned();

            let copy = root.join("copy");
            clone_into(&from, &copy).await?;
            anyhow::ensure!(copy.join(&rel).is_file(), "work copy is missing the file");

            let checked = root.join("check");
            let to = checked.to_string_lossy().into_owned();
            git_isolated(
                Path::new("."),
                &["clone", "--quiet", "--no-hardlinks", "--", &from, &to],
                null_device(),
            )
            .await?;
            anyhow::ensure!(
                checked.join(&rel).is_file(),
                "check copy is missing the file"
            );
            anyhow::Ok(())
        }
        .await;
        let _ = std::fs::remove_dir_all(&root);
        r
    }

    /// The leak check against real git: values in a file's contents, a
    /// binary file, a file name and an author field are all found, and a key
    /// that is not there is not reported.
    #[tokio::test]
    async fn leak_check_reads_the_agents_commits() -> anyhow::Result<()> {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("git not found; skipping");
            return Ok(());
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let data =
            std::env::temp_dir().join(format!("decurion-leak-{}-{nanos}", std::process::id()));
        let dir = data.join("workspaces").join("demo-7");
        std::fs::create_dir_all(&dir)?;
        let branch = "decurion/7";
        let r = async {
            git(&dir, &["init", "--quiet"]).await?;
            // A publishable key the app already ships with.
            std::fs::write(dir.join("public.js"), "pk = 'public-test-value-1234';\n")?;
            git(&dir, &["add", "."]).await?;
            commit(&dir, "t <t@example.invalid>", "base").await?;
            std::fs::write(base_file(&dir), git(&dir, &["rev-parse", "HEAD"]).await?)?;
            git(&dir, &["switch", "--quiet", "-c", branch]).await?;
            let keys = |names: &[&str]| -> Vec<(String, String)> {
                names
                    .iter()
                    .map(|n| {
                        (
                            n.to_string(),
                            format!("{}-test-value-1234", n.to_lowercase()),
                        )
                    })
                    .collect()
            };
            // A clean commit first: nothing is found, even though it changes
            // the file holding a key that was there before the agent.
            std::fs::write(dir.join("app.txt"), "uses $STRIPE_SECRET_KEY by name\n")?;
            std::fs::write(
                dir.join("public.js"),
                "pk = 'public-test-value-1234';\nmore();\n",
            )?;
            git(&dir, &["add", "."]).await?;
            commit(&dir, "t <t@example.invalid>", "clean").await?;
            assert_eq!(new_commits(&data, &dir, branch, None).await?, 1);
            let all = keys(&["STRIPE", "BLOB", "FILE", "AUTHOR", "UNUSED", "PUBLIC"]);
            assert!(leaked_keys(&data, &dir, branch, None, &all)
                .await?
                .is_empty());
            assert!(leaked_keys(&data, &dir, branch, None, &[])
                .await?
                .is_empty());

            std::fs::write(dir.join("config.env"), "KEY=stripe-test-value-1234\n")?;
            let mut blob = vec![0u8, 159, 146, 150];
            blob.extend_from_slice(b"blob-test-value-1234");
            std::fs::write(dir.join("data.bin"), blob)?;
            std::fs::write(dir.join("file-test-value-1234-é"), "x")?;
            git(&dir, &["add", "."]).await?;
            commit(&dir, "author-test-value-1234 <a@example.invalid>", "leak").await?;
            assert_eq!(new_commits(&data, &dir, branch, None).await?, 2);
            assert_eq!(
                leaked_keys(&data, &dir, branch, None, &all).await?,
                ["STRIPE", "BLOB", "FILE", "AUTHOR"]
            );

            // A hand-made commit whose message hides a value after a NUL,
            // which `git log` would stop printing at.
            let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]).await?;
            let parent = git(&dir, &["rev-parse", "HEAD"]).await?;
            let raw = format!(
                "tree {tree}\nparent {parent}\nauthor t <t@example.invalid> 0 +0000\n\
                 committer t <t@example.invalid> 0 +0000\n\nhello\0message-test-value-1234\n"
            );
            let mut p = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args([
                    "hash-object",
                    "-t",
                    "commit",
                    "-w",
                    "--literally",
                    "--stdin",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?;
            {
                use std::io::Write;
                p.stdin.take().context("stdin")?.write_all(raw.as_bytes())?;
            }
            let out = p.wait_with_output()?;
            let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            git(&dir, &["update-ref", &format!("refs/heads/{branch}"), &id]).await?;
            assert_eq!(new_commits(&data, &dir, branch, None).await?, 3);
            let with_message = keys(&["STRIPE", "MESSAGE"]);
            assert_eq!(
                leaked_keys(&data, &dir, branch, None, &with_message).await?,
                ["STRIPE", "MESSAGE"]
            );
            anyhow::Ok(())
        }
        .await;
        let _ = std::fs::remove_dir_all(&data);
        r
    }

    /// git with a fixed author, for the tests' own commits and merges.
    async fn tgit(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
        let mut all = vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ];
        all.extend_from_slice(args);
        git(dir, &all).await
    }

    fn temp_root(what: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        std::env::temp_dir().join(format!("decurion-{what}-{}-{nanos}", std::process::id()))
    }

    /// A Bring up to date run's copy gets the local branch decurion-main at
    /// exactly the commit the claim names, from the starting bundle; a
    /// bundle without it, or with another commit, stops the run plainly.
    #[tokio::test]
    async fn an_update_copy_gets_decurion_main_from_the_bundle_or_does_not_start(
    ) -> anyhow::Result<()> {
        let root = temp_root("main-copy");
        std::fs::create_dir_all(&root)?;
        let r = async {
            let src = root.join("src");
            std::fs::create_dir_all(&src)?;
            git(&src, &["init", "--quiet"]).await?;
            commit(&src, "t <t@example.invalid>", "a").await?;
            let a = git(&src, &["rev-parse", "HEAD"]).await?;
            commit(&src, "t <t@example.invalid>", "main").await?;
            let m = git(&src, &["rev-parse", "HEAD"]).await?;
            git(&src, &["update-ref", "refs/heads/decurion-base", &a]).await?;
            git(&src, &["update-ref", "refs/heads/decurion-main", &m]).await?;
            let both = root.join("both.bundle").to_string_lossy().into_owned();
            let one = root.join("one.bundle").to_string_lossy().into_owned();
            git(
                &src,
                &[
                    "bundle",
                    "create",
                    "--quiet",
                    &both,
                    "refs/heads/decurion-base",
                    "refs/heads/decurion-main",
                ],
            )
            .await?;
            git(
                &src,
                &[
                    "bundle",
                    "create",
                    "--quiet",
                    &one,
                    "refs/heads/decurion-base",
                ],
            )
            .await?;
            let clone = |bundle: String, name: &str| {
                let to = root.join(name).to_string_lossy().into_owned();
                async move {
                    git(
                        Path::new("."),
                        &["clone", "--quiet", "--no-checkout", "--", &bundle, &to],
                    )
                    .await?;
                    anyhow::Ok(PathBuf::from(to))
                }
            };
            let good = clone(both.clone(), "good").await?;
            prepare_main(&good, &m).await?;
            assert_eq!(
                git(&good, &["rev-parse", "refs/heads/decurion-main"]).await?,
                m
            );
            let missing = clone(one, "missing").await?;
            let err = prepare_main(&missing, &m).await.expect_err("no main");
            assert!(err.to_string().contains("did not start"), "{err}");
            let other = clone(both, "other").await?;
            assert!(prepare_main(&other, &a).await.is_err());
            assert!(git(
                &other,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "refs/heads/decurion-main"
                ]
            )
            .await
            .is_err());
            anyhow::Ok(())
        }
        .await;
        let _ = std::fs::remove_dir_all(&root);
        r
    }

    /// The checks at the end of a Bring up to date run, against real git.
    /// Main brings a test key of its own and the agent resolves a conflict
    /// in the merge: only the agent's commits are counted and scanned, so
    /// nothing is reported. A key the merge itself adds, in a resolved
    /// file, a binary file or the merge's message, is. A merge of anything
    /// but main, an octopus merge, or no merge at all fails the shape check.
    #[tokio::test]
    async fn after_a_merge_only_the_agents_own_work_is_counted_and_scanned() -> anyhow::Result<()> {
        let data = temp_root("merge-check");
        let dir = data.join("workspaces").join("demo-8");
        std::fs::create_dir_all(&dir)?;
        let branch = "decurion/8";
        let r = async {
            git(&dir, &["init", "--quiet", "-b", "main"]).await?;
            std::fs::write(dir.join("app.txt"), "one\ntwo\n")?;
            tgit(&dir, &["add", "."]).await?;
            tgit(&dir, &["commit", "--quiet", "-m", "base"]).await?;
            let base = git(&dir, &["rev-parse", "HEAD"]).await?;
            std::fs::write(base_file(&dir), &base)?;
            // Main moves on: a conflicting line, and a key of its own.
            std::fs::write(dir.join("app.txt"), "one on main\ntwo\n")?;
            std::fs::write(dir.join("main.env"), "KEY=mainkey-test-value-1234\n")?;
            tgit(&dir, &["add", "."]).await?;
            tgit(&dir, &["commit", "--quiet", "-m", "main"]).await?;
            let m = git(&dir, &["rev-parse", "HEAD"]).await?;
            // The pull request's work.
            tgit(&dir, &["switch", "--quiet", "-c", branch, &base]).await?;
            std::fs::write(dir.join("app.txt"), "one on the branch\ntwo\n")?;
            tgit(&dir, &["commit", "--quiet", "-am", "feature"]).await?;
            let feature = git(&dir, &["rev-parse", "HEAD"]).await?;
            let keys = |names: &[&str]| -> Vec<(String, String)> {
                names
                    .iter()
                    .map(|n| {
                        (
                            n.to_string(),
                            format!("{}-test-value-1234", n.to_lowercase()),
                        )
                    })
                    .collect()
            };
            let all = keys(&["MAINKEY", "RESOLVED", "BLOB", "MESSAGE", "UNUSED"]);
            // A merge whose conflict is resolved by the agent.
            let merge = |resolved: &'static str, extra: bool, message: &'static str| {
                let dir = dir.clone();
                let m = m.clone();
                let feature = feature.clone();
                async move {
                    tgit(&dir, &["reset", "--quiet", "--hard", &feature]).await?;
                    let _ = tgit(&dir, &["merge", "--no-ff", "--no-edit", &m]).await;
                    std::fs::write(dir.join("app.txt"), resolved)?;
                    if extra {
                        let mut blob = vec![0u8, 159, 146, 150];
                        blob.extend_from_slice(b"blob-test-value-1234");
                        std::fs::write(dir.join("data.bin"), blob)?;
                    }
                    tgit(&dir, &["add", "-A"]).await?;
                    tgit(&dir, &["commit", "--quiet", "-m", message]).await?;
                    git(&dir, &["rev-parse", "HEAD"]).await
                }
            };
            merge("one on main\nand the branch\ntwo\n", false, "Merge main").await?;
            assert!(merged_main(&data, &dir, branch, &m).await?);
            // The feature and the merge; main's own commit is not the agent's.
            assert_eq!(new_commits(&data, &dir, branch, Some(&m)).await?, 2);
            assert_eq!(new_commits(&data, &dir, branch, None).await?, 3);
            assert!(leaked_keys(&data, &dir, branch, Some(&m), &all)
                .await?
                .is_empty());
            // Scanned as ordinary work, main's key would stop it: the
            // difference is only what main already held.
            assert_eq!(
                leaked_keys(&data, &dir, branch, None, &all).await?,
                ["MAINKEY"]
            );

            merge(
                "one on main\nresolved-test-value-1234\ntwo\n",
                true,
                "Merge main message-test-value-1234",
            )
            .await?;
            assert!(merged_main(&data, &dir, branch, &m).await?);
            let mut found = leaked_keys(&data, &dir, branch, Some(&m), &all).await?;
            found.sort();
            assert_eq!(found, ["BLOB", "MESSAGE", "RESOLVED"]);

            // No merge at all: the branch does not hold main.
            tgit(&dir, &["reset", "--quiet", "--hard", &feature]).await?;
            assert!(!merged_main(&data, &dir, branch, &m).await?);
            // Holds main, but through a merge of something else on top of it.
            tgit(&dir, &["switch", "--quiet", "-c", "later", &m]).await?;
            tgit(&dir, &["commit", "--quiet", "--allow-empty", "-m", "later"]).await?;
            let later = git(&dir, &["rev-parse", "HEAD"]).await?;
            tgit(&dir, &["switch", "--quiet", branch]).await?;
            let tree = git(&dir, &["rev-parse", &format!("{later}^{{tree}}")]).await?;
            let sideways = tgit(
                &dir,
                &[
                    "commit-tree",
                    &tree,
                    "-p",
                    &feature,
                    "-p",
                    &later,
                    "-m",
                    "merge",
                ],
            )
            .await?;
            tgit(
                &dir,
                &["update-ref", &format!("refs/heads/{branch}"), &sideways],
            )
            .await?;
            assert!(!merged_main(&data, &dir, branch, &m).await?);
            // An octopus merge that includes main.
            let octopus = tgit(
                &dir,
                &[
                    "commit-tree",
                    &tree,
                    "-p",
                    &feature,
                    "-p",
                    &m,
                    "-p",
                    &later,
                    "-m",
                    "octopus",
                ],
            )
            .await?;
            tgit(
                &dir,
                &["update-ref", &format!("refs/heads/{branch}"), &octopus],
            )
            .await?;
            assert!(!merged_main(&data, &dir, branch, &m).await?);
            anyhow::Ok(())
        }
        .await;
        let _ = std::fs::remove_dir_all(&data);
        r
    }
}
