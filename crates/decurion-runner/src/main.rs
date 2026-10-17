use anyhow::{bail, Context};
use decurion_proto::{Claim, MoveRequest, RunEvent, ThreadState};
use decurion_runner::{
    container_name, over_budget, parse_line, pi_run_args, prompt_for, run_args, Credential, PiRun,
    BUDGET_STOP, DOCKER_CLI_ENV,
};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

const RUNNER: &str = "local";

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
/// token (`claude setup-token`, saved as `decurion/claude-profile-A`), the
/// pay-per-use API key (`decurion/anthropic-api`), or the OpenAI or xAI key
/// (`decurion/openai-api`, `decurion/xai-api`).
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
    let tmp_s = tmp.to_string_lossy().into_owned();
    git(
        Path::new("."),
        &[
            "clone",
            "--quiet",
            "--no-hardlinks",
            "--",
            &c.app.repo_path,
            &tmp_s,
        ],
    )
    .await?;
    git(&tmp, &["switch", "--quiet", "-c", &branch]).await?;
    let base = git(&tmp, &["rev-parse", "HEAD"]).await?;
    std::fs::write(base_file(&dir), base)?;
    // Only a fully prepared clone gets the real name, so a crash mid-setup
    // never leaves a half-made workspace that looks finished.
    std::fs::rename(&tmp, &dir)?;
    Ok(dir)
}

/// Where the starting commit is kept: next to the workspace, outside the
/// container's mount, so the agent cannot move it.
fn base_file(dir: &Path) -> PathBuf {
    dir.with_extension("base")
}

/// Commits the agent added on its branch. The branch is fetched into a
/// runner-owned bare repo with hooks off and counted there against the
/// recorded starting commit; nothing the agent wrote is trusted except the
/// commits themselves.
async fn new_commits(data: &Path, dir: &Path, branch: &str) -> anyhow::Result<u64> {
    let base = std::fs::read_to_string(base_file(dir)).context("missing base commit")?;
    let name = dir.file_name().context("workspace name")?;
    let bare = data.join("review").join(name).with_extension("git");
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

/// Reads Claude Code's output to the end, posting its events, and returns its
/// result and whether anything came after it.
async fn claude_output<R: tokio::io::AsyncBufRead + Unpin>(
    hub: &Hub,
    id: i64,
    out: &mut R,
) -> std::io::Result<(Option<RunEvent>, bool)> {
    // Claude Code's result is the last line it prints. Anything after it, or a
    // container that does not exit cleanly, means the agent may have printed a
    // fake result: its cost is then not trusted and the hub charges the whole
    // amount set aside.
    let mut result: Option<RunEvent> = None;
    let mut after_result = false;
    while let Some(line) = read_capped_line(out).await? {
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
    Ok((result, after_result))
}

/// Reads pi's output to the end, posting its events, and returns its result,
/// if it has one, and whether the runner stopped it at its budget.
///
/// pi has no spending limit of its own, so the runner keeps the running total
/// of what pi reports and removes the container once it reaches the budget.
/// The hub held back part of the money set aside for the call in flight.
async fn pi_output<R: tokio::io::AsyncBufRead + Unpin>(
    hub: &Hub,
    id: i64,
    out: &mut R,
    budget_usd: Option<f64>,
) -> std::io::Result<(Option<RunEvent>, bool)> {
    let mut run = PiRun::default();
    let mut stopped = false;
    while let Some(line) = read_capped_line(out).await? {
        let events = run.parse_line(&line);
        if stopped {
            // Still counted: money spent on a call that was in flight.
            continue;
        }
        for e in &events {
            hub.event(id, e).await;
        }
        if over_budget(run.cost_usd(), budget_usd) {
            stopped = true;
            tracing::info!(id, cost = run.cost_usd(), "stopping a run at its budget");
            let _ = docker()
                .args(["rm", "-f", &container_name(id)])
                .output()
                .await;
        }
    }
    Ok(if stopped {
        (Some(run.budget_stop()), true)
    } else {
        (run.finished(), false)
    })
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
    let cred = Credential::for_account(c.account);
    let pi = cred.pi_model().is_some();
    if pi && c.budget_usd.is_none() {
        // The runner is pi's only spending limit.
        bail!("{} work came without a budget", c.account.label());
    }
    let args = if pi {
        pi_run_args(id, &wt, &session, cred)?
    } else {
        run_args(id, &wt, &session, c.resume, cred, c.budget_usd)?
    };
    let token = secret(cred)?;
    let prompt = prompt_for(&c.thread.prompt, c.resume);
    // Tell the hub before launching: from here the run may cost money, and if
    // the hub cannot be told, the run does not start.
    hub.post(&format!("/threads/{id}/events"), &RunEvent::Started)
        .await
        .context("could not tell the hub the run is starting")?;
    let mut child = docker()
        .env(cred.var(), token)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("could not start docker")?;

    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child.stdin.take().context("no stdin")?;
        stdin.write_all(prompt.as_bytes()).await?;
        // Dropping stdin closes it, which tells the agent the task is complete.
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
    let (result, after_result, budget_stopped) = if pi {
        let (result, stopped) = pi_output(hub, id, &mut out, c.budget_usd).await?;
        (result, false, stopped)
    } else {
        let (result, after_result) = claude_output(hub, id, &mut out).await?;
        (result, after_result, false)
    };
    let status = child.wait().await?;
    let finished = match result {
        Some(RunEvent::Finished {
            ok,
            summary,
            cost_usd,
            input_tokens,
            output_tokens,
        }) => {
            // The runner stopped a run at its budget itself, so its container
            // does not exit cleanly, but what pi reported until then stands.
            let trusted = (status.success() || budget_stopped) && !after_result;
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
