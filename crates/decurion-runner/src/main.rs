use anyhow::{bail, Context};
use decurion_proto::goal::{HelperClaim, HelperResult, HelperSource};
use decurion_proto::{App, Claim, MoveRequest, RunEvent, ThreadState};
use decurion_runner::gatekeeper::{Gatekeeper, Grant, ANTHROPIC_API};
use decurion_runner::{
    check_home_volume, container_name, design_home_volume, goal_home_volume, helper_container_name,
    helper_run_args, parse_line, parse_result, prompt_for, read_page, run_args, split_result,
    valid_branch, Credential, FirstResult, Sandbox, BUDGET_STOP, DOCKER_CLI_ENV, LIMIT_STOP,
};
use reqwest::StatusCode;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

const RUNNER: &str = "local";

/// The hub answered, but not with success.
#[derive(Debug)]
struct HubRefused {
    path: String,
    status: StatusCode,
    body: String,
}

impl std::fmt::Display for HubRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "hub {}: {} {}", self.path, self.status, self.body)
    }
}

impl std::error::Error for HubRefused {}

/// The status the hub answered with, if the error is the hub saying no.
fn hub_status(err: &anyhow::Error) -> Option<StatusCode> {
    err.downcast_ref::<HubRefused>().map(|e| e.status)
}

/// Cheap to clone: the work loop and the helper loop each hold one.
#[derive(Clone)]
struct Hub {
    base: String,
    key: String,
    http: reqwest::Client,
}

impl Hub {
    async fn post<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> anyhow::Result<reqwest::Response> {
        let r = self
            .http
            .post(format!("{}/api{path}", self.base))
            .bearer_auth(&self.key)
            .json(body)
            .send()
            .await?;
        let status = r.status();
        if !status.is_success() {
            return Err(HubRefused {
                path: path.to_owned(),
                status,
                body: r.text().await.unwrap_or_default(),
            }
            .into());
        }
        Ok(r)
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
        .await
        .map(|_| ())
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

/// Like [`git`], but ignoring the PC's system and user git settings, for
/// commands that handle files an agent wrote.
async fn git_isolated(dir: &Path, args: &[&str], null: &str) -> anyhow::Result<String> {
    let out = Command::new("git")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null)
        .env("GIT_LFS_SKIP_SMUDGE", "1")
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

/// The thread's own copy of the code, on its own branch. Reused on resume.
///
/// A full clone, not a `git worktree`: a worktree's `.git` file points at the
/// main repo's folder on the host, which does not exist inside the container.
/// `--no-hardlinks` matters: a hardlinked object file is the same file as the
/// one in the app's own repo, so an agent could overwrite it in place.
///
/// Once an agent has run in this folder it controls its git config and hooks,
/// so the runner never runs git in it again (see [`new_commits`]).
async fn workspace(data: &Path, c: &Claim) -> anyhow::Result<PathBuf> {
    let dir = data
        .join("workspaces")
        .join(format!("{}-{}", c.app.name, c.thread.id));
    let branch = c.thread.branch.clone().context("claim has no branch")?;
    if dir.join(".git").is_dir() {
        // Read the file instead of asking git, which would run the agent's config.
        let head = std::fs::read_to_string(dir.join(".git").join("HEAD")).unwrap_or_default();
        if head.trim() != format!("ref: refs/heads/{branch}") {
            bail!("workspace {} is not on branch {branch}", dir.display());
        }
        return Ok(dir);
    }
    std::fs::create_dir_all(dir.parent().context("workspace parent")?)?;
    let tmp = dir.with_extension("partial");
    let _ = std::fs::remove_dir_all(&tmp);
    clone_into(&c.app.repo_path, &tmp, None).await?;
    git(&tmp, &["switch", "--quiet", "-c", &branch]).await?;
    let base = git(&tmp, &["rev-parse", "HEAD"]).await?;
    std::fs::write(base_file(&dir), base)?;
    // Only a fully prepared clone gets the real name, so a crash mid-setup
    // never leaves a half-made workspace that looks finished.
    std::fs::rename(&tmp, &dir)?;
    Ok(dir)
}

/// `git clone --no-hardlinks`, optionally of one branch. See [`workspace`]
/// for why hardlinks are off.
async fn clone_into(from: &str, to: &Path, branch: Option<&str>) -> anyhow::Result<()> {
    let to_s = to.to_string_lossy().into_owned();
    let mut args = vec!["clone", "--quiet", "--no-hardlinks"];
    if let Some(b) = branch {
        args.extend(["--branch", b]);
    }
    args.extend(["--", from, &to_s]);
    git(Path::new("."), &args).await.map(|_| ())
}

/// Where the starting commit is kept: next to the workspace, outside the
/// container's mount, so the agent cannot move it.
fn base_file(dir: &Path) -> PathBuf {
    dir.with_extension("base")
}

/// The runner's own bare copy of a workspace's commits.
fn review_repo(data: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
    let name = dir.file_name().context("workspace name")?;
    Ok(data.join("review").join(name).with_extension("git"))
}

/// Commits the agent added on its branch. The branch is fetched into a
/// runner-owned bare repo with hooks off and counted there against the
/// recorded starting commit; nothing the agent wrote is trusted except the
/// commits themselves.
async fn new_commits(data: &Path, dir: &Path, branch: &str) -> anyhow::Result<u64> {
    let base = std::fs::read_to_string(base_file(dir)).context("missing base commit")?;
    let bare = review_repo(data, dir)?;
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
    let range = format!("{}..refs/heads/{branch}", base.trim());
    let n = git(&bare, &["rev-list", "--count", &range]).await?;
    Ok(n.parse()?)
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

/// Starts an agent's container and hands it its prompt on stdin.
fn spawn_agent(env: ContainerEnv, args: Vec<String>, prompt: String) -> anyhow::Result<Child> {
    use tokio::io::AsyncWriteExt;
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
    // Written from its own task, so the caller can read stdout and stderr
    // meanwhile: a big prompt and a chatty docker could otherwise each wait
    // on the other's full pipe.
    tokio::spawn(async move {
        if let Err(err) = stdin.write_all(prompt.as_bytes()).await {
            tracing::warn!(%err, "could not hand the agent its prompt");
        }
        // Dropping stdin closes it, which tells Claude the task is complete.
    });
    Ok(child)
}

/// Drains stderr alongside stdout so a chatty container cannot fill the pipe
/// and stall; keeps only its tail.
fn stderr_tail(child: &mut Child) -> anyhow::Result<JoinHandle<String>> {
    let stderr = child.stderr.take().context("no stderr")?;
    Ok(tokio::spawn(async move {
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
    }))
}

/// Variables set on the docker process for a container to take with `-e`.
type ContainerEnv = Vec<(&'static str, String)>;

/// The host-side gatekeeper and the address containers reach it at.
#[derive(Clone)]
struct Gate {
    keeper: Gatekeeper,
    url: String,
}

impl Gate {
    /// The environment for a container's credential. A pay-per-use container
    /// gets a token for the gatekeeper, never the key; the grant is revoked
    /// when it drops, so the caller keeps it until the container is gone.
    fn credential(&self, cred: Credential) -> anyhow::Result<(Option<Grant>, ContainerEnv)> {
        if cred == Credential::PayPerUse && !self.keeper.is_alive() {
            // Without the gatekeeper the run could only fail, and a failed
            // pay-per-use run is charged its whole budget. Stop before starting.
            bail!("the gatekeeper has stopped; restart the runner before running pay-per-use work");
        }
        let token = secret(cred)?;
        let (grant, values) = match cred {
            Credential::PayPerUse => {
                let g = self.keeper.grant(token);
                let v = vec![g.token().to_owned(), self.url.clone()];
                (Some(g), v)
            }
            Credential::Max(_) => (None, vec![token]),
        };
        // Names come from the same list run_args passes to docker with `-e`.
        anyhow::ensure!(
            values.len() == cred.vars().len(),
            "credential variables out of step"
        );
        Ok((grant, cred.vars().iter().copied().zip(values).collect()))
    }
}

/// Runs one claimed thread to the end and reports the outcome.
async fn run(hub: &Hub, data: &Path, gate: &Gate, c: Claim) -> anyhow::Result<()> {
    let id = c.thread.id;
    let session = c
        .thread
        .session_id
        .clone()
        .context("claim has no session")?;
    // Only one copy of a thread may ever run: clear any container left behind
    // by a runner that died.
    let _ = docker()
        .args(["rm", "-f", &container_name(id)])
        .output()
        .await;

    let wt = workspace(data, &c).await?;
    let cred = if c.budget_usd.is_some() {
        Credential::PayPerUse
    } else {
        Credential::Max(c.app.profile)
    };
    let (grant, env) = gate.credential(cred)?;
    let prompt = prompt_for(&c.thread.prompt, c.resume, c.follow_up.as_deref());
    // Tell the hub before launching: from here the run may cost money, and if
    // the hub cannot be told, the run does not start.
    hub.post(&format!("/threads/{id}/events"), &RunEvent::Started)
        .await
        .context("could not tell the hub the run is starting")?;
    let args = run_args(id, &wt, &session, c.resume, cred, c.budget_usd)?;
    let mut child = spawn_agent(env, args, prompt)?;
    let stderr_task = stderr_tail(&mut child)?;
    let stdout = child.stdout.take().context("no stdout")?;
    let mut out = BufReader::new(stdout);
    // Claude Code prints one result, as its last line. A second result, output
    // after it, or a container that does not exit cleanly means the agent may
    // have printed a fake one: the first still counts, but its cost is not
    // trusted and the hub charges the whole amount set aside.
    // A usage-limit report is held the same way: an agent could print a fake
    // one to freeze its account, so only a result that stands alone counts.
    let mut watch = FirstResult::default();
    while let Some(line) = read_capped_line(&mut out).await? {
        let (board, result) = split_result(parse_line(&line));
        for e in board {
            hub.event(id, &e).await;
        }
        watch.push(&line, result);
    }
    let status = child.wait().await?;
    // The container is gone; its token goes with it.
    drop(grant);
    let alone = watch.trusted();
    let (result, limit) = match watch.result {
        Some(r) => (Some(r.finished), r.limit),
        None => (None, None),
    };
    let finished = match result {
        Some(RunEvent::Finished {
            ok,
            summary,
            cost_usd,
            input_tokens,
            output_tokens,
        }) => {
            let trusted = status.success() && alone;
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
    let max_limit = limit.filter(|_| alone && c.budget_usd.is_none());
    let err = stderr_task.await.unwrap_or_default();
    // The agent's own "done" is not enough: work only goes to review if it
    // actually left commits on its branch.
    let (to, note) = match finished {
        Some((true, _)) if status.success() => {
            match new_commits(data, &wt, c.thread.branch.as_deref().unwrap_or_default()).await {
                Ok(0) => (
                    ThreadState::Failed,
                    "agent said it finished but saved no commits".to_owned(),
                ),
                Ok(n) => (
                    ThreadState::InReview,
                    format!("agent finished with {n} new commit(s); ready for review"),
                ),
                Err(e) => (ThreadState::Failed, format!("could not check commits: {e}")),
            }
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
    if let (ThreadState::Queued, Some(reset_epoch)) = (to, max_limit) {
        // The hub saves the outage and requeues the work in one step.
        hub.post(
            &format!("/threads/{id}/events"),
            &RunEvent::LimitHit { reset_epoch },
        )
        .await
        .map(|_| ())
    } else {
        hub.move_to(id, to, &note).await
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
            HelperSource::App { goal_id } => (
                data.join("goals").join(format!("{app}-{goal_id}")),
                true,
                goal_home_volume(*goal_id),
                false,
            ),
            HelperSource::AppCopy { goal_id } => (
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
            let _ = docker()
                .args(["volume", "rm", "-f", &self.home_volume])
                .output()
                .await;
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
    let tmp = dir.with_extension("partial");
    let _ = std::fs::remove_dir_all(&tmp);
    clone_into(&app.repo_path, &tmp, None).await?;
    // Only a finished clone gets the real name, as in `workspace`.
    std::fs::rename(&tmp, dir)?;
    Ok(())
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
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let hooks = format!("core.hooksPath={null}");
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
    let _ = docker().args(["rm", "-f", &name]).output().await;
    hb.clean().await;
    let r = helper_container(hub, data, gate, job, &name, &hb).await;
    // `--rm` only applies if the docker client lived to see the exit.
    let _ = docker().args(["rm", "-f", &name]).output().await;
    hb.clean().await;
    r
}

/// The longest a helper job may run before it is stopped.
const HELPER_TIME_LIMIT: Duration = Duration::from_secs(60 * 60);

async fn helper_container(
    hub: &Hub,
    data: &Path,
    gate: &Gate,
    job: &HelperClaim,
    name: &str,
    hb: &HelperBox,
) -> anyhow::Result<HelperResult> {
    match &job.source {
        HelperSource::App { .. } => goal_copy(&job.app, &hb.dir).await?,
        HelperSource::AppCopy { .. } => {
            std::fs::create_dir_all(hb.dir.parent().context("design copy parent")?)?;
            clone_into(&job.app.repo_path, &hb.dir, None).await?;
        }
        HelperSource::Branch { thread_id, branch } => {
            branch_copy(data, &job.app, *thread_id, branch, &hb.dir).await?
        }
    }
    if let Some(out) = &hb.out {
        std::fs::create_dir_all(out)?;
    }
    let cred = if job.budget_usd.is_some() {
        Credential::PayPerUse
    } else {
        Credential::Max(job.app.profile)
    };
    // Held until this function returns, after the container is gone.
    let (_grant, env) = gate.credential(cred)?;
    let args = helper_run_args(
        &Sandbox {
            name,
            src: &hb.dir,
            read_only: hb.read_only,
            home_volume: &hb.home_volume,
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
    let mut child = spawn_agent(env, args, job.prompt.clone())?;
    let stderr_task = stderr_tail(&mut child)?;
    let mut out = BufReader::new(child.stdout.take().context("no stdout")?);
    // Same trust rule as `run`, except that two results fail the job: a cost
    // only counts from Claude's one result in a container that exited cleanly.
    let ran = tokio::time::timeout(HELPER_TIME_LIMIT, async {
        let mut watch = FirstResult::default();
        while let Some(line) = read_capped_line(&mut out).await? {
            let parsed = parse_result(&line);
            watch.push(&line, parsed);
        }
        let status = child.wait().await?;
        anyhow::Ok((watch.helper_answer(), status))
    })
    .await;
    let Ok(ran) = ran else {
        let _ = docker().args(["rm", "-f", name]).output().await;
        let _ = child.kill().await;
        stderr_task.abort();
        return Ok(HelperResult {
            ok: false,
            text: format!(
                "The job took too long and was stopped after {} minutes.",
                HELPER_TIME_LIMIT.as_secs() / 60
            ),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            page_html: None,
        });
    };
    let (result, status) = ran?;
    let err = stderr_task.await.unwrap_or_default();
    let mut r = match result {
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
        None => HelperResult {
            ok: false,
            text: format!(
                "container ended without a result ({status}): {}",
                err.trim()
            ),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            page_html: None,
        },
    };
    // Make sure the container is gone, so the page cannot change while it is
    // read, even if the docker client exited before the container did.
    let _ = docker().args(["rm", "-f", name]).output().await;
    if let Some(dir) = &hb.out {
        r.page_html = read_page(dir);
    }
    Ok(r)
}

/// How long a helper job's end keeps being retried while the hub is down
/// or failing.
/// Short enough that a hub stuck failing does not hold up every goal's
/// helpers for long.
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
            None => err
                .downcast_ref::<reqwest::Error>()
                .is_some_and(|e| !e.is_builder()),
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
async fn helper_loop(hub: Hub, data: PathBuf, gate: Gate) {
    loop {
        let r = hub
            .post(
                "/runner/helper/claim",
                &serde_json::json!({ "runner": RUNNER }),
            )
            .await;
        match r {
            Ok(resp) if resp.status() == reqwest::StatusCode::NO_CONTENT => {
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            Ok(resp) => {
                let job: HelperClaim = match resp.json().await {
                    Ok(j) => j,
                    Err(err) => {
                        // The hub requeues it when this runner restarts.
                        tracing::error!(%err, "could not read claimed helper job");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                };
                let id = job.job_id;
                tracing::info!(id, kind = job.kind.as_str(), "helper job");
                let result = match run_helper(&hub, &data, &gate, &job).await {
                    Ok(r) => r,
                    Err(err) => {
                        tracing::error!(%err, id, "helper job failed");
                        HelperResult {
                            ok: false,
                            text: format!("runner error: {err:#}"),
                            cost_usd: None,
                            input_tokens: None,
                            output_tokens: None,
                            page_html: None,
                        }
                    }
                };
                finish_helper(&hub, id, result).await;
            }
            Err(err) => {
                tracing::warn!(%err, "hub unreachable for helper jobs; retrying");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

/// Throwaway helper copies, homes and pages left by a runner that died.
/// Their jobs come back from the hub and start fresh.
async fn clean_helper_leftovers(data: &Path) {
    for d in ["checks", "designs", "out"] {
        remove_dir(&data.join(d));
    }
    if let Ok(out) = docker().args(["volume", "ls", "-q"]).output().await {
        let vols: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .filter(|v| {
                v.starts_with("decurion-check-home-") || v.starts_with("decurion-design-home-")
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "decurion_runner=info".into()),
        )
        .init();
    let data = data_dir()?;
    // One runner per hub: a second one would requeue and kill the first one's
    // work. The OS releases the lock if this process dies.
    let lock = std::fs::File::create(data.join("runner.lock"))?;
    if lock.try_lock().is_err() {
        bail!("another Decurion runner is already running on this PC");
    }
    // Docker Desktop forwards host.docker.internal to the host's loopback, so
    // containers reach the gatekeeper while nothing off this PC can.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("could not start the gatekeeper")?;
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
    let keys: serde_json::Value = serde_json::from_slice(
        &std::fs::read(data.join("keys.json"))
            .context("start the hub first; it creates keys.json")?,
    )?;
    let hub = Hub {
        base: std::env::var("DECURION_HUB").unwrap_or_else(|_| "http://127.0.0.1:7420".into()),
        key: keys["runner"]
            .as_str()
            .context("keys.json has no runner key")?
            .to_owned(),
        // A stalled hub must not hold up the end of a run (and with it the
        // revoking of its gatekeeper token).
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?,
    };
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
    clean_helper_leftovers(&data).await;
    let paused: Vec<i64> = hub
        .post("/runner/hello", &serde_json::json!({ "runner": RUNNER }))
        .await?
        .json()
        .await?;
    if !paused.is_empty() {
        tracing::info!(
            ?paused,
            "requeued work this runner had in flight; it will resume"
        );
    }
    tokio::spawn(helper_loop(hub.clone(), data.clone(), gate.clone()));
    loop {
        let r = hub
            .post("/runner/claim", &serde_json::json!({ "runner": RUNNER }))
            .await;
        match r {
            Ok(resp) if resp.status() == reqwest::StatusCode::NO_CONTENT => {
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
                tracing::info!(id, resume = claim.resume, "running");
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
                    let _ = hub.move_to(id, ThreadState::Failed, &err.to_string()).await;
                }
            }
            Err(err) => {
                tracing::warn!(%err, "hub unreachable; retrying");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}
