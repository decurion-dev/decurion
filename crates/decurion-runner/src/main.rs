use anyhow::{bail, Context};
use decurion_proto::{
    Claim, HelloReply, JoinReply, JoinRequest, Live, MoveRequest, NewCheckpoint, Place,
    RunCredential, RunEvent, RunnerHello, ThreadState, RULES_VERSION,
};
use decurion_runner::gatekeeper::{Gatekeeper, Grant, ANTHROPIC_API};
use decurion_runner::handover::{
    bundle_spec, host_git_args, host_git_env, is_sha, move_note, reuse_workspace, save_edits_args,
    session_export_args, session_import_args, tool_container_name, CODE_CAP, SESSION_CAP,
};
use decurion_runner::identity::{
    hub_url, place_from_env, write_atomic, CloudRunner, PC_HUB, PC_NAME,
};
use decurion_runner::{
    backoff, container_name, hide, hide_cut, home_volume, is_interrupted_turn, is_turn_start,
    parse_line_hiding, part_done, prompt_for, retriable, run_args, sibling, Acks, Action,
    Conversation, Credential, LeakScanner, Start, Step, BUDGET_STOP, DOCKER_CLI_ENV, LIMIT_STOP,
};
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

/// The app's test keys as (name, value), from this PC's key store. The hub
/// sends only their names.
fn test_keys(hub: &Hub, data: &Path, c: &Claim) -> anyhow::Result<Vec<(String, String)>> {
    // Test keys live only in the PC's key store, which never leaves it.
    if hub.place == Place::Cloud && !c.test_keys.is_empty() {
        bail!(
            "this app uses test keys, which stay on the PC, so its work cannot run on the \
cloud worker yet. Move it to the PC and try again."
        );
    }
    let vault = Vault::system(data);
    c.test_keys
        .iter()
        .map(|name| {
            // The hub checked the name when it was saved; check it again here,
            // where it becomes an environment variable.
            check_name(name).map_err(|e| anyhow::anyhow!("test key {name:?}: {e}"))?;
            let value = vault
                .get(c.app.id, name)
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
    set_aside(&dir)?;
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

/// The runner-owned bare repo the agent's commits are fetched into.
fn review_repo(data: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
    let name = dir.file_name().context("workspace name")?;
    Ok(sibling(&data.join("review").join(name), "git"))
}

/// The agent's commits in the review repo: from the recorded start to its
/// branch.
fn new_range(dir: &Path, branch: &str) -> anyhow::Result<String> {
    let base = std::fs::read_to_string(base_file(dir)).context("missing base commit")?;
    Ok(format!("{}..refs/heads/{branch}", base.trim()))
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
async fn leaked_keys(
    data: &Path,
    dir: &Path,
    branch: &str,
    keys: &[(String, String)],
) -> anyhow::Result<Vec<String>> {
    if keys.is_empty() {
        return Ok(vec![]);
    }
    let bare = review_repo(data, dir)?;
    let range = new_range(dir, branch)?;
    let objects = git(
        &bare,
        &["rev-list", "--objects", "--no-object-names", &range, "--"],
    )
    .await?;
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
        let room = MAX_LINE.saturating_sub(buf.len());
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
                let events = parse_line_hiding(&line, guard.secrets);
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

/// Removes a thread's container and checks that it is really gone. Tried a
/// few times: docker refuses to remove a container it is still taking down.
async fn remove_container(id: i64) -> bool {
    let name = container_name(id);
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        match docker_out(&["rm", "-f", &name]).await {
            // Fails when there is no such container too; the check says.
            Ok(out) if !out.status.success() => tracing::debug!(
                id,
                stderr = %String::from_utf8_lossy(&out.stderr).trim(),
                "docker rm failed"
            ),
            Ok(_) => {}
            Err(err) => tracing::warn!(%err, id, "could not run docker rm"),
        }
        let filter = format!("name=^{name}$");
        match docker_out(&["ps", "-aq", "--filter", &filter]).await {
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

/// Runs one claimed thread to the end and reports the outcome. `keys` are the
/// app's test keys: they go into the container, and their values are hidden
/// from everything sent to the hub.
async fn run(
    hub: &Hub,
    data: &Path,
    gate: &Gate,
    c: Claim,
    keys: &[(String, String)],
) -> anyhow::Result<()> {
    let id = c.thread.id;
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
            // The gatekeeper prices every answer and stops the run's calls
            // once its set-aside money is used.
            let budget = c.reserved_micros.map(|m| m.max(0) as u64);
            let g = gate.keeper.grant_with_budget(token, budget);
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
    for (name, value) in keys {
        cmd.env(name, value);
    }
    let secrets: Vec<String> = keys.iter().map(|(_, v)| v.clone()).collect();
    let prompt = prompt_for(
        &c.thread.prompt,
        start,
        &c.test_keys,
        &c.messages,
        c.interrupted.as_ref(),
    );
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
            &c.test_keys,
        )?)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("could not start docker")?;

    // Stdin stays open so owner messages can follow the prompt. The claim's
    // undelivered messages are in the prompt and are acked once Claude has
    // read it.
    let (conv, first) = Conversation::start(&prompt, &c.messages);
    let (wrote_tx, wrote) = mpsc::unbounded_channel();
    let (to_agent, writer) = spawn_writer(id, child.stdin.take().context("no stdin")?, wrote_tx);
    let _ = to_agent.send(first);
    let stderr_task = spawn_stderr_tail(child.stderr.take().context("no stderr")?, secrets.clone());
    let (lines, reader) = spawn_reader(child.stdout.take().context("no stdout")?);

    let (result, limit, after_result) = match follow(
        hub,
        id,
        conv,
        to_agent,
        lines,
        wrote,
        Guard {
            secrets: &secrets,
            grant: grant.as_ref(),
        },
    )
    .await
    {
        Ended::Stopped => {
            // The owner already moved it; the hub has the last word. Nothing
            // is saved: the work is not this runner's to touch any more.
            tracing::info!(id, "stopped by the owner");
            kill(hub, &mut child, id).await;
            let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
            return Ok(());
        }
        Ended::Moved(to) => {
            hand_over(hub, data, id, &wt, &branch, to, &mut child).await;
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
    let used_up = grant.as_ref().is_some_and(|g| g.used_up());
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
        }) => {
            let trusted = status.success() && !after_result;
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
                    },
                )
                .await;
            }
            used_up.then_some((false, summary))
        }
    };
    let max_limit = limit.filter(|_| !after_result && c.budget_usd.is_none());
    let err = stderr_task.await.unwrap_or_default();
    let exit = Exit {
        status,
        stderr: &err,
        pay_per_use,
        used_up,
    };
    let (to, note) = outcome(data, &wt, &c, finished, max_limit, exit, keys).await;
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

/// Where the work goes after the run. The agent's own "done" is not enough:
/// work only goes to review if it actually left commits on its branch, and
/// none of them holds a test key.
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
        Some((true, _)) if status.success() => match new_commits(data, wt, branch).await {
            Ok(0) => (
                ThreadState::Failed,
                "agent said it finished but saved no commits".to_owned(),
            ),
            Ok(n) => match leaked_keys(data, wt, branch, keys).await {
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
                stderr.trim()
            ),
        ),
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
        },
    )
    .await;
    let _ = with_retry("report the failure", FINAL_RETRY, async || {
        hub.move_to(id, ThreadState::Failed, note).await
    })
    .await;
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
    // containers reach the host at the Docker bridge's own address (run_args
    // maps host.docker.internal there), which only containers and the host
    // itself can use.
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
            .post(
                "/runner/claim",
                &serde_json::json!({ "runner": hub.name, "rules_version": RULES_VERSION }),
            )
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
                // Read here, not in `run`, so its errors can be hidden too.
                let keys = match test_keys(&hub, &data, &claim) {
                    Ok(k) => k,
                    Err(err) => {
                        tracing::error!(%err, id, "could not read test keys");
                        fail(&hub, id, &err.to_string()).await;
                        continue;
                    }
                };
                let secrets: Vec<String> = keys.iter().map(|(_, v)| v.clone()).collect();
                if let Err(err) = run(&hub, &data, &gate, claim, &keys).await {
                    let note = hide(&err.to_string(), &secrets);
                    tracing::error!(err = %note, id, "run failed");
                    fail(&hub, id, &note).await;
                }
            }
            // The hub was updated while this runner was waiting for work.
            Err(HubError::Refused(_, s, text)) if s == StatusCode::CONFLICT => {
                match serde_json::from_str::<HelloReply>(&text) {
                    Ok(reply) => match rules_differ(&data, &reply)? {},
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
            assert_eq!(new_commits(&data, &dir, branch).await?, 1);
            let all = keys(&["STRIPE", "BLOB", "FILE", "AUTHOR", "UNUSED", "PUBLIC"]);
            assert!(leaked_keys(&data, &dir, branch, &all).await?.is_empty());
            assert!(leaked_keys(&data, &dir, branch, &[]).await?.is_empty());

            std::fs::write(dir.join("config.env"), "KEY=stripe-test-value-1234\n")?;
            let mut blob = vec![0u8, 159, 146, 150];
            blob.extend_from_slice(b"blob-test-value-1234");
            std::fs::write(dir.join("data.bin"), blob)?;
            std::fs::write(dir.join("file-test-value-1234-é"), "x")?;
            git(&dir, &["add", "."]).await?;
            commit(&dir, "author-test-value-1234 <a@example.invalid>", "leak").await?;
            assert_eq!(new_commits(&data, &dir, branch).await?, 2);
            assert_eq!(
                leaked_keys(&data, &dir, branch, &all).await?,
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
            assert_eq!(new_commits(&data, &dir, branch).await?, 3);
            let with_message = keys(&["STRIPE", "MESSAGE"]);
            assert_eq!(
                leaked_keys(&data, &dir, branch, &with_message).await?,
                ["STRIPE", "MESSAGE"]
            );
            anyhow::Ok(())
        }
        .await;
        let _ = std::fs::remove_dir_all(&data);
        r
    }
}
