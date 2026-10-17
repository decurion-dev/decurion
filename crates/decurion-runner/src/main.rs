use anyhow::{bail, Context};
use decurion_proto::{Claim, MoveRequest, RunEvent, ThreadState};
use decurion_runner::gatekeeper::{Gatekeeper, ANTHROPIC_API};
use decurion_runner::{
    container_name, hide, hide_cut, parse_line_hiding, prompt_for, run_args, Credential,
    LeakScanner, BUDGET_STOP, DOCKER_CLI_ENV, LIMIT_STOP,
};
use decurion_vault::{check_name, Vault};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
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
fn test_keys(data: &Path, c: &Claim) -> anyhow::Result<Vec<(String, String)>> {
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
    let bare = review_repo(data, dir)?;
    if !bare.exists() {
        std::fs::create_dir_all(&bare)?;
        git(&bare, &["init", "--quiet", "--bare"]).await?;
    }
    let src = dir.to_string_lossy().into_owned();
    let refspec = format!("+refs/heads/{branch}:refs/heads/{branch}");
    // The source repo's config is agent-written. Command-line settings win
    // over it and reach git's child processes: no hooks, no fsmonitor, no
    // transports beyond plain local files, and no lazy fetching of missing
    // objects through a planted remote.
    let hooks = no_hooks();
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
    let n = git(&bare, &["rev-list", "--count", &new_range(dir, branch)?]).await?;
    Ok(n.parse()?)
}

/// The runner-owned bare repo the agent's commits are fetched into.
fn review_repo(data: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
    let name = dir.file_name().context("workspace name")?;
    Ok(data.join("review").join(name).with_extension("git"))
}

/// The agent's commits in the review repo: from the recorded start to its
/// branch.
fn new_range(dir: &Path, branch: &str) -> anyhow::Result<String> {
    let base = std::fs::read_to_string(base_file(dir)).context("missing base commit")?;
    Ok(format!("{}..refs/heads/{branch}", base.trim()))
}

fn no_hooks() -> String {
    let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
    format!("core.hooksPath={null}")
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
        .env("GIT_NO_LAZY_FETCH", "1")
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

/// The host-side gatekeeper and the address containers reach it at.
struct Gate {
    keeper: Gatekeeper,
    url: String,
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
    if cred == Credential::PayPerUse && !gate.keeper.is_alive() {
        // Without the gatekeeper the run could only fail, and a failed
        // pay-per-use run is charged its whole budget. Stop before starting.
        bail!("the gatekeeper has stopped; restart the runner before running pay-per-use work");
    }
    let token = secret(cred)?;
    let mut cmd = docker();
    // A pay-per-use container gets a token for the gatekeeper, never the key.
    // The grant is revoked when it drops, on every way out of this function.
    let (grant, values) = match cred {
        Credential::PayPerUse => {
            // The gatekeeper prices every answer and stops the run's calls
            // once its set-aside money is used.
            let budget = c
                .reserved_usd
                .map(|usd| (usd.max(0.0) * 1_000_000.0) as u64);
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
    let prompt = prompt_for(&c.thread.prompt, c.resume, &c.test_keys);
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
            c.resume,
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

    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child.stdin.take().context("no stdin")?;
        stdin.write_all(prompt.as_bytes()).await?;
        // Dropping stdin closes it, which tells Claude the task is complete.
    }

    // Drain stderr alongside stdout so a chatty container cannot fill the pipe
    // and stall; keep only its tail. Each line is hidden before it goes in, so
    // trimming the tail cannot cut a value in half.
    let stderr = child.stderr.take().context("no stderr")?;
    let stderr_secrets = secrets.clone();
    let stderr_task = tokio::spawn(async move {
        let mut tail = String::new();
        let mut r = BufReader::new(stderr);
        while let Ok(Some((line, cut))) = read_capped_line(&mut r).await {
            let line = if cut {
                hide_cut(&line, &stderr_secrets)
            } else {
                hide(&line, &stderr_secrets)
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
    });
    let stdout = child.stdout.take().context("no stdout")?;
    let mut out = BufReader::new(stdout);
    // Claude Code's result is the last line it prints. Anything after it, or a
    // container that does not exit cleanly, means the agent may have printed a
    // fake result: its cost is then not trusted and the hub charges the whole
    // amount set aside.
    let mut result: Option<RunEvent> = None;
    // A usage-limit report is held the same way: an agent could print a fake
    // one to freeze its account, so only Claude's real, final result counts.
    let mut limit: Option<Option<i64>> = None;
    let mut after_result = false;
    while let Some((line, _)) = read_capped_line(&mut out).await? {
        if result.is_some() && !line.trim().is_empty() {
            after_result = true;
        }
        let events = parse_line_hiding(&line, &secrets);
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
    let status = child.wait().await?;
    // The container is gone; its token goes with it.
    // For pay-per-use, the gatekeeper's count is the cost: it saw every call,
    // including any the agent made itself, and runs outside the container.
    let metered_usd = grant
        .as_ref()
        .map(|g| c.prior_usd + g.spent_micros() as f64 / 1_000_000.0);
    let used_up = grant.as_ref().is_some_and(|g| g.used_up());
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
                    cost_usd: metered_usd.or(cost_usd.filter(|_| trusted)),
                    input_tokens,
                    output_tokens,
                },
            )
            .await;
            Some((ok, summary))
        }
        _ => {
            // No result line at all: still report what the gatekeeper saw.
            if let Some(cost) = metered_usd {
                hub.event(
                    id,
                    &RunEvent::Finished {
                        ok: false,
                        summary: String::new(),
                        cost_usd: Some(cost),
                        input_tokens: None,
                        output_tokens: None,
                    },
                )
                .await;
            }
            None
        }
    };
    let max_limit = limit.filter(|_| !after_result && c.budget_usd.is_none());
    let err = stderr_task.await.unwrap_or_default();
    // The agent's own "done" is not enough: work only goes to review if it
    // actually left commits on its branch, and none of them holds a test key.
    let branch = c.thread.branch.as_deref().unwrap_or_default();
    let (to, note) = match finished {
        Some((true, _)) if status.success() => match new_commits(data, &wt, branch).await {
            Ok(0) => (
                ThreadState::Failed,
                "agent said it finished but saved no commits".to_owned(),
            ),
            Ok(n) => match leaked_keys(data, &wt, branch, keys).await {
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
        hub.move_to(id, to, &hide(&note, &secrets)).await
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
    let _ = hub.move_to(id, ThreadState::Failed, note).await;
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
                // Read here, not in `run`, so its errors can be hidden too.
                let keys = match test_keys(&data, &claim) {
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
            Err(err) => {
                tracing::warn!(%err, "hub unreachable; retrying");
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
