use anyhow::{bail, Context};
use decurion_proto::{
    Claim, HelloReply, JoinReply, JoinRequest, MoveRequest, NewCheckpoint, Place, RunCredential,
    RunEvent, RunnerHello, Thread, ThreadState, RULES_VERSION,
};
use decurion_runner::gatekeeper::{Gatekeeper, ANTHROPIC_API};
use decurion_runner::handover::{
    bundle_spec, is_sha, move_note, reuse_workspace, save_edits_args, session_export_args,
    session_import_args, tool_container_name, CODE_CAP, SESSION_CAP,
};
use decurion_runner::identity::{
    hub_url, place_from_env, write_atomic, CloudRunner, PC_HUB, PC_NAME,
};
use decurion_runner::{
    backoff, container_name, home_volume, parse_line, prompt_for, retriable, run_args, sibling,
    Credential, Start, BUDGET_STOP, DOCKER_CLI_ENV, LIMIT_STOP,
};
use reqwest::header::{HeaderMap, CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::StatusCode;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// How long the last steps of a run (saving a checkpoint, reporting the
/// outcome) keep trying while the hub is out of reach. A cloud worker's hub is
/// the owner's PC, which may be asleep; the work must not be dropped.
const FINAL_RETRY: Duration = Duration::from_secs(60 * 60);
/// How long a download keeps trying. The hub just handed out the work, so it
/// is awake; this only covers a short hiccup.
const DOWNLOAD_RETRY: Duration = Duration::from_secs(2 * 60);
/// How often a running thread's place is checked, to notice a move.
const WATCH_EVERY: Duration = Duration::from_secs(5);
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

struct Hub {
    base: String,
    key: String,
    http: reqwest::Client,
    /// This runner's name, bound by the hub to its key.
    name: String,
    /// The computer this runner is; it only runs work placed here.
    place: Place,
}

/// Short calls to the hub give up after this, so a hub that went to sleep
/// mid-call does not hang the runner.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

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

    async fn get(&self, path: &str) -> Result<reqwest::Response, HubError> {
        let req = self.http.get(self.url(path)).timeout(CALL_TIMEOUT);
        self.send(path, req).await
    }

    async fn event(&self, thread: i64, e: &RunEvent) {
        if let Err(err) = self.post(&format!("/threads/{thread}/events"), e).await {
            tracing::warn!(%err, thread, "could not post event");
        }
    }

    async fn move_to(&self, thread: i64, to: ThreadState, note: &str) -> anyhow::Result<()> {
        self.post(
            &format!("/threads/{thread}/move"),
            &MoveRequest {
                to,
                note: Some(note.to_owned()),
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
        tracing::warn!(%err, what, wait_secs = wait.as_secs(), "hub unreachable; trying again");
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
/// token (`claude setup-token`, saved as `decurion/claude-profile-A`) or the
/// pay-per-use API key (`decurion/anthropic-api`), which stays with the
/// gatekeeper.
fn secret(cred: Credential) -> anyhow::Result<String> {
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

async fn git(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
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
    remove_all(&dir)?;
    remove_all(&marker_file)?;
    remove_all(&base_file(&dir))?;
    std::fs::create_dir_all(dir.parent().context("workspace parent")?)?;
    let tmp = sibling(&dir, "partial");
    remove_all(&tmp)?;
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

/// Where the starting commit is kept: next to the workspace, outside the
/// container's mount, so the agent cannot move it.
fn base_file(dir: &Path) -> PathBuf {
    sibling(dir, "base")
}

/// Fetches the agent's branch into a runner-owned bare repo with hooks off.
/// Nothing the agent wrote is trusted except the commits themselves.
async fn fetch_branch(data: &Path, dir: &Path, branch: &str) -> anyhow::Result<PathBuf> {
    let name = dir.file_name().context("workspace name")?;
    let bare = sibling(&data.join("review").join(name), "git");
    if !bare.exists() {
        std::fs::create_dir_all(&bare)?;
        git(&bare, &["init", "--quiet", "--bare"]).await?;
    }
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let src = dir.to_string_lossy().into_owned();
    let refspec = format!("+refs/heads/{branch}:refs/heads/{branch}");
    // The source repo's config is agent-written. Command-line settings win
    // over it and reach git's child processes: no hooks, no fsmonitor, no
    // transports beyond plain local files, and no lazy fetching of missing
    // objects through a planted remote.
    let hooks = format!("core.hooksPath={null}");
    git(
        &bare,
        &[
            "-c",
            &hooks,
            "-c",
            "core.fsmonitor=false",
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
/// repo against the recorded starting commit.
async fn new_commits(data: &Path, dir: &Path, branch: &str) -> anyhow::Result<u64> {
    let base = read_base(dir)?;
    let bare = fetch_branch(data, dir, branch).await?;
    count_since(&bare, &base, branch).await
}

async fn count_since(bare: &Path, base: &str, branch: &str) -> anyhow::Result<u64> {
    let range = format!("{base}..refs/heads/{branch}");
    let n = git(bare, &["rev-list", "--count", &range]).await?;
    Ok(n.parse()?)
}

/// Runs a throwaway container to the end, within [`TOOL_TIMEOUT`].
async fn run_tool(id: i64, args: &[String], stdin: Stdio) -> anyhow::Result<std::process::Output> {
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
            let _ = docker()
                .args(["rm", "-f", &tool_container_name(id)])
                .output()
                .await;
            bail!("throwaway container took too long")
        }
    }
}

/// Saves the agent's unfinished edits as a commit, inside a sealed throwaway
/// container, so they travel with the work.
async fn save_edits(id: i64, wt: &Path) -> anyhow::Result<()> {
    let [add, commit] = save_edits_args(id, wt)?;
    let out = run_tool(id, &add, Stdio::null()).await?;
    if !out.status.success() {
        bail!(
            "git add in the agent's copy failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let out = run_tool(id, &commit, Stdio::null()).await?;
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
    let out = run_tool(id, &session_import_args(id), Stdio::from(f)).await?;
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

const MAX_LINE: usize = 1 << 20;
const STDERR_TAIL: usize = 8 << 10;

/// Reads one line, keeping at most `MAX_LINE` bytes of it, so a container
/// cannot make the runner hold an unbounded line in memory.
async fn read_capped_line<R: tokio::io::AsyncBufRead + Unpin>(
    r: &mut R,
) -> std::io::Result<Option<String>> {
    let mut buf = Vec::new();
    let mut seen_any = false;
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
        let room = MAX_LINE.saturating_sub(buf.len());
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
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
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

/// Waits until the owner moves this running work to another computer, and
/// says where to.
async fn watch_place(hub: &Hub, id: i64) -> Place {
    loop {
        tokio::time::sleep(WATCH_EVERY).await;
        let t = match hub.get(&format!("/threads/{id}")).await {
            Ok(r) => r.json::<Thread>().await.map_err(anyhow::Error::from),
            Err(e) => Err(e.into()),
        };
        match t {
            Ok(t) if t.place != hub.place => return t.place,
            Ok(_) => {}
            Err(err) => tracing::debug!(%err, id, "could not check where the work belongs"),
        }
    }
}

/// Hands running work to the other computer: stop the agent, keep its
/// unfinished edits as a commit, save a checkpoint and put the work back in
/// the queue for the other runner. Never reports the work as failed: every
/// problem here is logged, and the work is queued either way.
async fn hand_over(
    hub: &Hub,
    data: &Path,
    id: i64,
    wt: &Path,
    branch: &str,
    to: Place,
    mut child: tokio::process::Child,
) {
    tracing::info!(id, to = to.as_str(), "work was moved; handing it over");
    let name = container_name(id);
    let _ = docker().args(["stop", "-t", "20", &name]).output().await;
    let _ = docker().args(["rm", "-f", &name]).output().await;
    let _ = child.kill().await;
    if let Err(err) = save_edits(id, wt).await {
        tracing::warn!(%err, id, "could not save unfinished edits");
    }
    if let Err(err) = save_checkpoint(hub, data, id, wt, branch).await {
        tracing::error!(%err, id, "could not save a checkpoint before moving");
    }
    let note = move_note(to);
    if let Err(err) = with_retry("hand the work over", FINAL_RETRY, async || {
        hub.move_to(id, ThreadState::Queued, note).await
    })
    .await
    {
        tracing::error!(%err, id, "could not put the moved work back in the queue");
    }
}

/// The host-side gatekeeper and the address containers reach it at.
struct Gate {
    keeper: Gatekeeper,
    url: String,
}

/// Runs one claimed thread to the end and reports the outcome.
async fn run(hub: &Hub, data: &Path, gate: &Gate, c: Claim) -> anyhow::Result<()> {
    let id = c.thread.id;
    let session = c
        .thread
        .session_id
        .clone()
        .context("claim has no session")?;
    let branch = c.thread.branch.clone().context("claim has no branch")?;
    // Only one copy of a thread may ever run: clear any container left behind
    // by a runner that died.
    let _ = docker()
        .args(["rm", "-f", &container_name(id), &tool_container_name(id)])
        .output()
        .await;

    let (wt, rebuilt) = workspace(hub, data, &c, &branch).await?;
    let start = Start::pick(c.resume, rebuilt, c.checkpoint.is_some());
    let cred = if c.budget_usd.is_some() {
        Credential::PayPerUse
    } else {
        Credential::Max(c.app.profile)
    };
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
            let g = gate.keeper.grant(token);
            let v = vec![g.token().to_owned(), gate.url.clone()];
            (Some(g), v)
        }
        Credential::Max(_) => (None, vec![token]),
    };
    // Names come from the same list run_args passes to docker with `-e`.
    anyhow::ensure!(
        values.len() == cred.vars().len(),
        "credential variables out of step"
    );
    for (name, value) in cred.vars().iter().zip(values) {
        cmd.env(name, value);
    }
    let prompt = prompt_for(&c.thread.prompt, start);
    // Tell the hub before launching: from here the run may cost money, and if
    // the hub cannot be told, the run does not start.
    hub.post(&format!("/threads/{id}/events"), &RunEvent::Started)
        .await
        .context("could not tell the hub the run is starting")?;
    let mut child = cmd
        .args(run_args(
            id,
            &wt,
            &session,
            start.resumes(),
            cred,
            c.budget_usd,
        )?)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("could not start docker")?;

    {
        let mut stdin = child.stdin.take().context("no stdin")?;
        stdin.write_all(prompt.as_bytes()).await?;
        // Dropping stdin closes it, which tells Claude the task is complete.
    }

    // Drain stderr alongside stdout so a chatty container cannot fill the pipe
    // and stall; keep only its tail.
    let stderr = child.stderr.take().context("no stderr")?;
    let stderr_task = tokio::spawn(async move {
        let mut tail = String::new();
        let mut r = BufReader::new(stderr);
        while let Ok(Some(line)) = read_capped_line(&mut r).await {
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
    });
    let stdout = child.stdout.take().context("no stdout")?;
    let mut out = BufReader::new(stdout);
    // Claude Code's result is the last line it prints. Anything after it, or a
    // container that does not exit cleanly, means the agent may have printed a
    // fake result: its cost is then not trusted and the hub charges the whole
    // amount set aside.
    let read = async {
        let mut result: Option<RunEvent> = None;
        // A usage-limit report is held the same way: an agent could print a fake
        // one to freeze its account, so only Claude's real, final result counts.
        let mut limit: Option<Option<i64>> = None;
        let mut after_result = false;
        while let Some(line) = read_capped_line(&mut out).await? {
            if result.is_some() && !line.trim().is_empty() {
                after_result = true;
            }
            let events = parse_line(&line);
            if events
                .iter()
                .any(|e| matches!(e, RunEvent::Finished { .. }))
            {
                limit = None;
            }
            for e in events {
                match e {
                    RunEvent::Finished { .. } => {
                        result = Some(e);
                        after_result = false;
                    }
                    RunEvent::LimitHit { reset_epoch } => limit = Some(reset_epoch),
                    _ => hub.event(id, &e).await,
                }
            }
        }
        anyhow::Ok((result, limit, after_result))
    };
    // While the agent works, watch for the owner moving the work elsewhere.
    let (result, limit, after_result) = tokio::select! {
        r = read => r?,
        to = watch_place(hub, id) => {
            hand_over(hub, data, id, &wt, &branch, to, child).await;
            return Ok(());
        }
    };
    let status = child.wait().await?;
    // The container is gone; its token goes with it.
    drop(grant);
    let finished = match result {
        Some(RunEvent::Finished {
            ok,
            summary,
            cost_usd,
            input_tokens,
            output_tokens,
        }) => {
            let trusted = status.success() && !after_result;
            hub.event(
                id,
                &RunEvent::Finished {
                    ok,
                    summary: summary.clone(),
                    cost_usd: cost_usd.filter(|_| trusted),
                    input_tokens,
                    output_tokens,
                },
            )
            .await;
            Some((ok, summary))
        }
        _ => None,
    };
    let max_limit = limit.filter(|_| !after_result && c.budget_usd.is_none());
    let err = stderr_task.await.unwrap_or_default();
    // The agent's own "done" is not enough: work only goes to review if it
    // actually left commits on its branch.
    let (to, note) = match finished {
        Some((true, _)) if status.success() => match new_commits(data, &wt, &branch).await {
            Ok(0) => (
                ThreadState::Failed,
                "agent said it finished but saved no commits".to_owned(),
            ),
            Ok(n) => (
                ThreadState::InReview,
                format!("agent finished with {n} new commit(s); ready for review"),
            ),
            Err(e) => (ThreadState::Failed, format!("could not check commits: {e}")),
        },
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
        Some((false, summary)) if summary == BUDGET_STOP => (ThreadState::NeedsYou, summary),
        Some((false, summary)) => (ThreadState::Failed, summary),
        None => (
            ThreadState::Failed,
            format!(
                "container ended without a result ({status}): {}",
                err.trim()
            ),
        ),
    };
    // Save the work whatever the outcome, so it can continue anywhere later.
    // A failed save is logged; the outcome is still reported.
    if let Err(err) = save_checkpoint(hub, data, id, &wt, &branch).await {
        tracing::error!(%err, id, "could not save a checkpoint");
    }
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
async fn hello(hub: &Hub, data: &Path) -> anyhow::Result<Vec<i64>> {
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
                return Ok(reply.paused);
            }
            Ok(r) if r.status() == StatusCode::CONFLICT => {
                let reply: HelloReply = r
                    .json()
                    .await
                    .context("hub refused this runner and gave no reason")?;
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

/// Port the gatekeeper listens on where it cannot pick a free one: on Linux
/// the cloud walls open exactly this port to containers.
const LINUX_GATE_PORT: u16 = 7421;

/// Starts the gatekeeper where only this computer's containers can reach it.
async fn start_gate() -> anyhow::Result<Gate> {
    // Docker Desktop (Windows, Mac) forwards host.docker.internal to the
    // host's loopback, so nothing off this computer can reach it. On Linux,
    // containers reach the host at the Docker bridge's own address (run_args
    // maps host.docker.internal there), which only containers and the host
    // itself can use.
    let addr = if cfg!(target_os = "linux") {
        std::net::SocketAddr::new(bridge_gateway().await?, LINUX_GATE_PORT)
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
    let gate = start_gate().await?;
    let place = place_from_env(std::env::var("DECURION_PLACE").ok().as_deref())?;
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
        .args(["ps", "-aq", "--filter", "name=^decurion-thread-"])
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
    let paused = hello(&hub, &data).await?;
    if !paused.is_empty() {
        tracing::info!(
            ?paused,
            "requeued work this runner had in flight; it will resume"
        );
    }
    loop {
        let r = hub
            .post("/runner/claim", &serde_json::json!({ "runner": hub.name }))
            .await;
        match r {
            Ok(resp) if resp.status() == StatusCode::NO_CONTENT => {
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            Ok(resp) => {
                let claim: Claim = match resp.json().await {
                    Ok(c) => c,
                    Err(err) => {
                        // The hub already marked it running; a runner restart
                        // will requeue it.
                        tracing::error!(%err, "could not read claimed work");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                };
                let id = claim.thread.id;
                tracing::info!(
                    id,
                    resume = claim.resume,
                    checkpoint = ?claim.checkpoint,
                    "running"
                );
                if let Err(err) = run(&hub, &data, &gate, claim).await {
                    tracing::error!(%err, id, "run failed");
                    hub.event(
                        id,
                        &RunEvent::Finished {
                            ok: false,
                            summary: err.to_string(),
                            cost_usd: None,
                            input_tokens: None,
                            output_tokens: None,
                        },
                    )
                    .await;
                    let note = err.to_string();
                    let _ = with_retry("report the failure", FINAL_RETRY, async || {
                        hub.move_to(id, ThreadState::Failed, &note).await
                    })
                    .await;
                }
            }
            Err(err) => {
                tracing::warn!(%err, "hub unreachable; retrying");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}
