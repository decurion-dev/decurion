use anyhow::{bail, Context};
use decurion_proto::{Claim, Live, MoveRequest, RunEvent, ThreadState};
use decurion_runner::gatekeeper::{Gatekeeper, ANTHROPIC_API};
use decurion_runner::{
    container_name, hide, hide_cut, is_interrupted_turn, is_turn_start, parse_line_hiding,
    part_done, prompt_for, run_args, Acks, Action, Conversation, Credential, LeakScanner, Step,
    BUDGET_STOP, DOCKER_CLI_ENV, LIMIT_STOP,
};
use decurion_vault::{check_name, Vault};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

const RUNNER: &str = "local";
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
}

impl Hub {
    /// What a run checks while it goes. Short timeout: a slow hub must not
    /// hold up reading the agent's output for long.
    async fn live(&self, thread: i64) -> anyhow::Result<Live> {
        let path = format!("/threads/{thread}/live");
        let r = self
            .http
            .get(format!("{}/api{path}", self.base))
            .bearer_auth(&self.key)
            .timeout(Duration::from_secs(10))
            .send()
            .await?;
        if !r.status().is_success() {
            bail!("hub {path}: {}", r.status());
        }
        Ok(r.json().await?)
    }

    async fn delivered(&self, thread: i64, ids: &[i64]) -> anyhow::Result<()> {
        self.post(
            &format!("/threads/{thread}/messages/delivered"),
            &serde_json::json!({ "ids": ids }),
        )
        .await
        .map(|_| ())
    }

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
                expect: None,
                receipt: None,
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
    /// The owner stopped the work while it ran.
    Stopped,
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
    secrets: &[String],
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
                let events = parse_line_hiding(&line, secrets);
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
                        RunEvent::Finished { ok, summary, .. } => {
                            (*ok, summary == BUDGET_STOP || summary == LIMIT_STOP)
                        }
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
    // Only one copy of a thread may ever run: clear any container left behind
    // by a runner that died, and do not start another while it may be there.
    if !remove_container(id).await {
        bail!(
            "could not remove the Docker container {} left from an earlier run; \
make sure Docker Desktop is running and delete that container there",
            container_name(id)
        );
    }

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
    for (name, value) in keys {
        cmd.env(name, value);
    }
    let secrets: Vec<String> = keys.iter().map(|(_, v)| v.clone()).collect();
    let prompt = prompt_for(
        &c.thread.prompt,
        c.resume,
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

    // Stdin stays open so owner messages can follow the prompt. The claim's
    // undelivered messages are in the prompt and are acked once Claude has
    // read it.
    let (conv, first) = Conversation::start(&prompt, &c.messages);
    let (wrote_tx, wrote) = mpsc::unbounded_channel();
    let (to_agent, writer) = spawn_writer(id, child.stdin.take().context("no stdin")?, wrote_tx);
    let _ = to_agent.send(first);
    let stderr_task = spawn_stderr_tail(child.stderr.take().context("no stderr")?, secrets.clone());
    let (lines, reader) = spawn_reader(child.stdout.take().context("no stdout")?);

    let (result, limit, after_result) =
        match follow(hub, id, conv, to_agent, lines, wrote, &secrets).await {
            Ended::Stopped => {
                // The owner already moved it; the hub has the last word.
                tracing::info!(id, "stopped by the owner");
                kill(hub, &mut child, id).await;
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
    let exit = Exit {
        status,
        stderr: &err,
    };
    let (to, note) = outcome(data, &wt, &c, finished, max_limit, exit, keys).await;
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

/// How the container ended: its exit status and the tail of its stderr.
struct Exit<'a> {
    status: ExitStatus,
    stderr: &'a str,
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
    let Exit { status, stderr } = exit;
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
        Some((false, summary)) if summary == BUDGET_STOP => (ThreadState::NeedsYou, summary),
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
