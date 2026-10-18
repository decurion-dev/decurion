//! "Run it" on the PC: starts an app's dev server in a sealed box of its own
//! and shows it to the owner in a browser box beside it.
//!
//! One preview at a time, taken from the hub like a sign-in window. The
//! preview gets a throwaway copy of the code (the piece of work's last saved
//! copy, or the app's own folder), its own closed network and gate with the
//! app's website list, a browser box the owner drives, and a box running the
//! app's start script with the app's test keys by name and no credential.
//! The runner tells the hub it is alive every few seconds and stops it as
//! soon as the hub says it should not run (the owner tapped Stop, started
//! another, or nobody has looked at it for a while). Everything is removed
//! when it stops: boxes, network, home volume and the copy of the code.
//!
//! When the app cannot start, or stops, the preview ends at once and the
//! hub keeps why, so the owner's screen says so (however late it opens) and,
//! when the app itself is why, offers to ask an agent to fix it. Nothing is
//! left running behind a page that only looks busy.

use super::{
    branch_copy, clone_into, docker, docker_out, read_capped_line, remove_dir, remove_named,
    start_site_gate, sync_review_copy, vault_keys, Hub, SiteGate, WINDING_DOWN,
};
use crate::browser_box::{self, PreviewBrowser};
use anyhow::Context;
use decurion_proto::browser::ToBrowser;
use decurion_proto::preview::{
    PreviewAlive, PreviewClaim, PreviewEnded, PreviewState, PREVIEW_ALIVE_SECS,
};
use decurion_runner::preview::{self, Recipe, Stage};
use decurion_runner::{
    hide, hide_cut, preview_box_args, preview_browser_name, preview_container_name,
    preview_home_volume, valid_branch, BoxNet,
};
use reqwest::StatusCode;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::Instant;

/// How long the app may take to say where it listens once it starts
/// installing. A first install can be slow on a PC.
const READY_WAIT: Duration = Duration::from_secs(15 * 60);
/// The hub out of reach for this long stops the preview: nobody can see it.
const HUB_GONE: Duration = Duration::from_secs(10 * 60);
/// Lines of the app's output kept to show when it stops.
const TAIL_LINES: usize = 15;
/// The longest line of the app's output kept.
const TAIL_LINE: usize = 200;
/// What the owner is told when the app's box ends by itself.
const STOPPED: &str = "Your app stopped before it was ready to use.";
/// What the owner is told when the app never says where to open it.
const NEVER_SAID: &str =
    "Your app didn't say where to open it within 15 minutes, so Decurion stopped it.";

/// Opens the previews the owner asks for, one at a time.
pub(crate) async fn preview_loop(hub: Hub, data: PathBuf) {
    let mut hub_down = false;
    loop {
        let Some(_job) = super::SideJob::start() else {
            return;
        };
        let r = hub
            .post("/runner/preview/claim", &serde_json::json!({}))
            .await;
        let claim: PreviewClaim = match r {
            Ok(resp) if resp.status() == StatusCode::NO_CONTENT => {
                hub_down = false;
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
            Ok(resp) => match resp.json().await {
                Ok(c) => c,
                Err(err) => {
                    tracing::warn!(%err, "could not read a preview from the hub");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }
            },
            // An older hub has no previews: ask now and then, quietly.
            Err(super::HubError::Refused(_, s, _)) if s == StatusCode::NOT_FOUND => {
                tokio::time::sleep(Duration::from_secs(60)).await;
                continue;
            }
            Err(err) => {
                if !hub_down {
                    tracing::warn!(%err, "could not ask the hub for previews");
                }
                hub_down = true;
                tokio::time::sleep(Duration::from_secs(10)).await;
                continue;
            }
        };
        hub_down = false;
        let sid = claim.session_id;
        tracing::info!(
            sid,
            app = %claim.app.name,
            thread = ?claim.thread.as_ref().map(|t| t.thread_id),
            "starting a preview"
        );
        let end = run_preview(&hub, &data, &claim).await;
        tracing::info!(sid, why = %end.why, failed = end.failed, "preview over");
        // Closes the session even when its relay never came up; the hub
        // keeps why for the owner's screen.

        if let Err(err) = hub.post(&format!("/runner/preview/{sid}/end"), &end).await {
            tracing::debug!(%err, sid, "could not tell the hub a preview ended");
        }
    }
}

/// Everything one preview holds, removed by [`Parts::clean`].
struct Parts {
    sid: i64,
    copy: PathBuf,
    gate: Option<SiteGate>,
    browser: Option<PreviewBrowser>,
}

impl Parts {
    async fn clean(mut self, why: &str) {
        let sid = self.sid;
        // The app's box first, so nothing is left serving to the browser.
        if !remove_named(sid, &[preview_container_name(sid)]).await {
            tracing::error!(sid, "could not remove a preview's box");
        }
        if let Some(b) = self.browser.take() {
            b.stop(why).await;
        }
        if let Some(g) = self.gate.take() {
            g.stop().await;
        }
        let _ = docker_out(&["volume", "rm", "-f", &preview_home_volume(sid)]).await;
        remove_dir(&self.copy);
    }
}

/// Runs one preview until it should stop. Returns why it stopped, for the
/// owner.
async fn run_preview(hub: &Hub, data: &Path, claim: &PreviewClaim) -> PreviewEnded {
    let sid = claim.session_id;
    let copy = data
        .join("previews")
        .join(format!("{}-{sid}", claim.app.name));
    // Left by a runner that died with this session in hand.
    let _ = remove_named(
        sid,
        &[preview_container_name(sid), preview_browser_name(sid)],
    )
    .await;
    let mut parts = Parts {
        sid,
        copy,
        gate: None,
        browser: None,
    };
    let end = match preview_in(hub, data, claim, &mut parts).await {
        Ok(why) => PreviewEnded {
            why,
            failed: false,
            fixable: false,
        },
        Err(err) => {
            tracing::warn!(err = %format!("{err:#}"), sid, "preview failed");
            let (why, fixable) = owner_words(&err);
            PreviewEnded {
                why,
                failed: true,
                fixable,
            }
        }
    };
    parts.clean(&end.why).await;
    end
}

/// An error for the owner, and whether the app itself is why: one we wrote
/// for them, or a plain catch-all.
fn owner_words(err: &anyhow::Error) -> (String, bool) {
    match err.downcast_ref::<ForOwner>() {
        Some(f) => (f.text.clone(), f.fixable),
        None => (
            "Your app could not be started. Make sure Docker is running on your PC, \
then tap Run it again."
                .to_owned(),
            false,
        ),
    }
}

/// An error written for the owner. `fixable` when the app itself is why (no
/// start recipe, or it stopped), so an agent could fix it.
#[derive(Debug)]
struct ForOwner {
    text: String,
    fixable: bool,
}

impl ForOwner {
    /// Something the owner sees to, like a missing test key.
    fn owner(text: impl Into<String>) -> Self {
        ForOwner {
            text: text.into(),
            fixable: false,
        }
    }

    /// The app's own problem, which an agent can fix.
    fn app(text: impl Into<String>) -> Self {
        ForOwner {
            text: text.into(),
            fixable: true,
        }
    }
}

impl std::fmt::Display for ForOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::error::Error for ForOwner {}

async fn preview_in(
    hub: &Hub,
    data: &Path,
    claim: &PreviewClaim,
    parts: &mut Parts,
) -> anyhow::Result<String> {
    let sid = claim.session_id;
    let keys = vault_keys(hub, data, claim.app.id, &claim.test_keys)
        .map_err(|e| ForOwner::owner(format!("{e}")))?;
    let secrets: Vec<String> = keys.iter().map(|(_, v)| v.clone()).collect();
    // The code and how it starts come first: an app that cannot start says
    // so in a second or two, before any box is made for it.
    copy_code(hub, data, claim, &parts.copy)
        .await
        .context("copying the code")?;
    let recipe = preview::recipe(&parts.copy).map_err(ForOwner::app)?;
    parts.gate = Some(
        start_site_gate(hub, BoxNet::preview(sid), &claim.app, None, None)
            .await
            .context("the preview's gate")?,
    );
    let browser = parts.browser.insert(
        browser_box::start_preview(hub, sid)
            .await
            .context("the preview's browser")?,
    );
    let names: Vec<String> = keys.iter().map(|(n, _)| n.clone()).collect();
    let args = preview_box_args(sid, &parts.copy, &names)?;
    let mut cmd = docker();
    for (name, value) in &keys {
        cmd.env(name, value);
    }
    let mut child = cmd
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("could not start docker")?;
    // The script goes in on stdin, and stdin is closed after it.
    {
        let mut stdin = child.stdin.take().context("no stdin")?;
        stdin
            .write_all(preview::script(&recipe).as_bytes())
            .await
            .context("the preview's box closed at once")?;
    }
    let (tx, mut lines) = mpsc::channel::<String>(256);
    for out in [
        child
            .stdout
            .take()
            .map(|o| Box::new(o) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
        child
            .stderr
            .take()
            .map(|e| Box::new(e) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
    ]
    .into_iter()
    .flatten()
    {
        let tx = tx.clone();
        let secrets = secrets.clone();
        tokio::spawn(async move {
            let mut r = BufReader::new(out);
            while let Ok(Some((line, cut))) = read_capped_line(&mut r).await {
                let line = if cut {
                    hide_cut(&line, &secrets)
                } else {
                    hide(&line, &secrets)
                };
                if tx.send(line).await.is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    follow(hub, sid, &recipe, browser, &mut child, &mut lines).await
}

fn note(text: &str) -> ToBrowser {
    ToBrowser::Note {
        text: text.to_owned(),
        stopped: false,
    }
}

/// The code the preview runs, in a folder of its own that is thrown away.
async fn copy_code(hub: &Hub, data: &Path, claim: &PreviewClaim, to: &Path) -> anyhow::Result<()> {
    remove_dir(to);
    std::fs::create_dir_all(to.parent().context("preview copy parent")?)?;
    match &claim.thread {
        // The piece of work as last saved, cloned from the runner's own
        // review repo with the same care as a checker's copy.
        Some(t) => {
            if !valid_branch(&t.branch) {
                anyhow::bail!("refusing odd branch name {:?}", t.branch);
            }
            let xfer = data
                .join("transfer")
                .join(format!("preview-{}", claim.session_id));
            sync_review_copy(
                hub,
                data,
                &claim.app,
                t.thread_id,
                &t.branch,
                t.checkpoint,
                &xfer,
            )
            .await?;
            branch_copy(data, &claim.app, t.thread_id, &t.branch, to).await
        }
        // The app as it is in its own folder on the PC.
        None => clone_into(&claim.app.repo_path, to).await,
    }
}

/// Follows the app's box until the preview should stop: passes progress to
/// the waiting page, opens the app once it says where it listens, and asks
/// the hub every few seconds whether to go on.
async fn follow(
    hub: &Hub,
    sid: i64,
    recipe: &Recipe,
    browser: &mut PreviewBrowser,
    child: &mut tokio::process::Child,
    lines: &mut mpsc::Receiver<String>,
) -> anyhow::Result<String> {
    let mut tail: VecDeque<String> = VecDeque::new();
    let mut opened = false;
    let mut out_open = true;
    let ready_by = Instant::now() + READY_WAIT;
    let mut alive = tokio::time::interval(Duration::from_secs(PREVIEW_ALIVE_SECS));
    alive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut hub_ok = Instant::now();
    let open = |browser: &PreviewBrowser, port: u16| {
        browser.send(note(preview::OPENING));
        browser.send(ToBrowser::Open {
            url: preview::url(port, &recipe.path),
        });
    };
    loop {
        tokio::select! {
            line = lines.recv(), if out_open => match line {
                Some(line) => {
                    match preview::stage(&line) {
                        Some(Stage::Installing) => browser.send(note(preview::INSTALLING)),
                        Some(Stage::Starting) => {
                            browser.send(note(preview::STARTING));
                            if let (Some(port), false) = (recipe.port, opened) {
                                opened = true;
                                open(browser, port);
                            }
                        }
                        None => {
                            if !opened {
                                if let Some(port) = preview::find_port(&line) {
                                    opened = true;
                                    open(browser, port);
                                }
                            }
                            keep(&mut tail, &line);
                        }
                    }
                }
                None => out_open = false,
            },
            status = child.wait() => {
                let code = status.ok().and_then(|s| s.code());
                tracing::info!(sid, ?code, "the app's box ended");
                // What it printed last, once the readers have caught up.
                let until = Instant::now() + Duration::from_secs(2);
                while let Ok(Some(line)) = tokio::time::timeout_at(until, lines.recv()).await {
                    if preview::stage(&line).is_none() {
                        keep(&mut tail, &line);
                    }
                }
                return Err(ForOwner::app(with_tail(STOPPED, &tail)).into());
            },
            _ = tokio::time::sleep_until(ready_by), if !opened => {
                let _ = child.start_kill();
                return Err(ForOwner::app(with_tail(NEVER_SAID, &tail)).into());
            },
            why = browser.ended() => {
                return Err(ForOwner::owner(format!("The preview's browser stopped. {why}")).into());
            },
            _ = alive.tick() => {
                if let Some(why) = still_wanted(hub, sid, &mut hub_ok).await {
                    return Ok(why);
                }
            },
        }
    }
}

/// Tells the hub the preview is alive. `Some(why)` once it should stop: the
/// hub said so, the runner is winding down, or the hub has been out of
/// reach too long.
async fn still_wanted(hub: &Hub, sid: i64, hub_ok: &mut Instant) -> Option<String> {
    if WINDING_DOWN.load(std::sync::atomic::Ordering::SeqCst) {
        return Some(
            "Stopped while Decurion updates on your PC. Tap Run it again in a minute.".to_owned(),
        );
    }
    match hub
        .post(
            &format!("/runner/preview/{sid}/alive"),
            &PreviewAlive::default(),
        )
        .await
    {
        Ok(resp) => {
            *hub_ok = Instant::now();
            match resp.json::<PreviewState>().await {
                Ok(PreviewState { open: false }) => return Some("Stopped.".to_owned()),
                Ok(_) => {}
                Err(err) => tracing::debug!(%err, sid, "odd answer from the hub"),
            }
        }
        Err(super::HubError::Refused(_, s, _)) if s.is_client_error() => {
            return Some("Stopped.".to_owned());
        }
        Err(err) => {
            if hub_ok.elapsed() > HUB_GONE {
                return Some("Stopped because your PC's hub stopped answering.".to_owned());
            }
            tracing::debug!(%err, sid, "could not reach the hub about a preview");
        }
    }
    None
}

/// Keeps the last lines of the app's output, each cut short.
fn keep(tail: &mut VecDeque<String>, line: &str) {
    let line = preview::strip_ansi(line);
    let line = line.trim_end();
    if line.is_empty() {
        return;
    }
    let cut: String = line.chars().take(TAIL_LINE).collect();
    tail.push_back(cut);
    while tail.len() > TAIL_LINES {
        tail.pop_front();
    }
}

/// What the owner is told, with the last lines the app printed if any.
fn with_tail(text: &str, tail: &VecDeque<String>) -> String {
    let mut text = text.to_owned();
    if !tail.is_empty() {
        text.push_str(" These are the last lines it printed:\n\n");
        text.push_str(&tail.iter().cloned().collect::<Vec<_>>().join("\n"));
    }
    text
}
