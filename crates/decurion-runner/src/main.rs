use anyhow::{bail, Context};
use decurion_proto::goal::{HelperClaim, HelperResult, HelperSource};
use decurion_proto::{App, Claim, MoveRequest, RunEvent, ThreadState};
use decurion_runner::{
    check_home_volume, container_name, design_home_volume, goal_home_volume, helper_container_name,
    helper_run_args, parse_line, parse_result, prompt_for, read_page, run_args, valid_branch,
    Credential, Sandbox, BUDGET_STOP, DOCKER_CLI_ENV,
};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

const RUNNER: &str = "local";

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
        if !r.status().is_success() {
            bail!(
                "hub {path}: {} {}",
                r.status(),
                r.text().await.unwrap_or_default()
            );
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
/// pay-per-use API key (`decurion/anthropic-api`).
fn secret(cred: Credential) -> anyhow::Result<String> {
    let target = cred.target();
    #[cfg(windows)]
    {
        let entry = keyring::Entry::new_with_target(&target, "decurion", "claude")?;
        entry
            .get_password()
            .with_context(|| format!("nothing saved in Credential Manager as {target}"))
    }
    #[cfg(not(windows))]
    {
        let var = cred.env_fallback();
        std::env::var(&var).with_context(|| format!("{var} not set ({target})"))
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
async fn spawn_agent(
    cred: Credential,
    token: String,
    args: Vec<String>,
    prompt: &str,
) -> anyhow::Result<Child> {
    use tokio::io::AsyncWriteExt;
    let mut child = docker()
        .env(cred.var(), token)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("could not start docker")?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    stdin.write_all(prompt.as_bytes()).await?;
    // Dropping stdin closes it, which tells Claude the task is complete.
    drop(stdin);
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

/// Runs one claimed thread to the end and reports the outcome.
async fn run(hub: &Hub, data: &Path, c: Claim) -> anyhow::Result<()> {
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
    let token = secret(cred)?;
    let prompt = prompt_for(&c.thread.prompt, c.resume, c.follow_up.as_deref());
    // Tell the hub before launching: from here the run may cost money, and if
    // the hub cannot be told, the run does not start.
    hub.post(&format!("/threads/{id}/events"), &RunEvent::Started)
        .await
        .context("could not tell the hub the run is starting")?;
    let args = run_args(id, &wt, &session, c.resume, cred, c.budget_usd)?;
    let mut child = spawn_agent(cred, token, args, &prompt).await?;
    let stderr_task = stderr_tail(&mut child)?;
    let stdout = child.stdout.take().context("no stdout")?;
    let mut out = BufReader::new(stdout);
    // Claude Code's result is the last line it prints. Anything after it, or a
    // container that does not exit cleanly, means the agent may have printed a
    // fake result: its cost is then not trusted and the hub charges the whole
    // amount set aside.
    let mut result: Option<RunEvent> = None;
    let mut after_result = false;
    while let Some(line) = read_capped_line(&mut out).await? {
        if result.is_some() && !line.trim().is_empty() {
            after_result = true;
        }
        for e in parse_line(&line) {
            if matches!(e, RunEvent::Finished { .. }) {
                result = Some(e);
                after_result = false;
            } else {
                hub.event(id, &e).await;
            }
        }
    }
    let status = child.wait().await?;
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
    hub.move_to(id, to, &note).await
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
    clone_into(&bare.to_string_lossy(), dir, Some(branch)).await?;
    // The last git command run here: the agent has not started yet.
    git(dir, &["branch", "decurion-base", base]).await?;
    Ok(())
}

/// Runs one coordinator, design or review job. Never posts events: what the
/// helper says only matters as its final answer, which the hub checks.
async fn run_helper(hub: &Hub, data: &Path, job: &HelperClaim) -> anyhow::Result<HelperResult> {
    let name = helper_container_name(job.job_id);
    let hb = HelperBox::for_job(data, job);
    // A job handed out again after a runner restart starts from scratch.
    let _ = docker().args(["rm", "-f", &name]).output().await;
    hb.clean().await;
    let r = helper_container(hub, data, job, &name, &hb).await;
    // `--rm` only applies if the docker client lived to see the exit.
    let _ = docker().args(["rm", "-f", &name]).output().await;
    hb.clean().await;
    r
}

async fn helper_container(
    hub: &Hub,
    data: &Path,
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
    let token = secret(cred)?;
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
    let mut child = spawn_agent(cred, token, args, &job.prompt).await?;
    let stderr_task = stderr_tail(&mut child)?;
    let mut out = BufReader::new(child.stdout.take().context("no stdout")?);
    // Same trust rule as `run`: a cost only counts from the last line of a
    // container that exited cleanly.
    let mut result: Option<HelperResult> = None;
    let mut after_result = false;
    while let Some(line) = read_capped_line(&mut out).await? {
        if result.is_some() && !line.trim().is_empty() {
            after_result = true;
        }
        if let Some(r) = parse_result(&line) {
            result = Some(r);
            after_result = false;
        }
    }
    let status = child.wait().await?;
    let err = stderr_task.await.unwrap_or_default();
    let mut r = match result {
        Some(mut r) => {
            if !status.success() || after_result {
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
    // The container is gone, so the page cannot change while it is read.
    if let Some(dir) = &hb.out {
        r.page_html = read_page(dir);
    }
    Ok(r)
}

/// Reports a helper job's end, trying again for a while if the hub is down.
/// If it never gets through, the hub hands the job out again once this
/// runner says hello after a restart.
async fn finish_helper(hub: &Hub, id: i64, r: &HelperResult) {
    let path = format!("/runner/helper/{id}/finish");
    let mut waits = [2, 4, 8].into_iter();
    loop {
        let Err(err) = hub.post(&path, r).await else {
            return;
        };
        // A hub that answered and said no will say no again.
        let unreachable = err.downcast_ref::<reqwest::Error>().is_some();
        match waits.next() {
            Some(secs) if unreachable => {
                tracing::warn!(%err, id, "could not report helper job; retrying");
                tokio::time::sleep(Duration::from_secs(secs)).await;
            }
            _ => {
                tracing::error!(%err, id, "could not report helper job");
                return;
            }
        }
    }
}

/// Takes coordinator, design and review jobs one at a time, alongside the
/// work loop, so a goal's planning never waits for a long build.
async fn helper_loop(hub: Hub, data: PathBuf) {
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
                let result = match run_helper(&hub, &data, &job).await {
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
                finish_helper(&hub, id, &result).await;
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
        http: reqwest::Client::new(),
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
    tokio::spawn(helper_loop(hub.clone(), data.clone()));
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
                if let Err(err) = run(&hub, &data, claim).await {
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
