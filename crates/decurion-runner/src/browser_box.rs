//! The test browser in a box of its own: a work run's, and the sign-in
//! window's. This starts the box, talks to it over its stdin and stdout,
//! and relays its screen to the hub, and the owner's clicks and typing back,
//! over a WebSocket the runner opens itself, only while someone watches.
//! What may go in and out is checked in `decurion_runner::browser`.

use super::{docker, read_line_up_to, remove_named, spawn_stderr_tail, Hub};
use anyhow::{bail, Context};
use decurion_proto::browser::{
    BrowserMode, BrowserWant, Controller, FromBrowser, RelayDown, RelayUp, SiteState, ToBrowser,
};
use decurion_runner::browser::{
    decode, encode, from_relay, relay_url, start_run as start_line, start_sign_in, to_relay,
    MAX_FROM_LINE, MIN_WIDTH, QUALITY,
};
use decurion_runner::{
    browser_box_args, browser_container_name, sign_in_box_args, sign_in_container_name,
};
use futures_util::{SinkExt, StreamExt};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HeaderValue, AUTHORIZATION};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

/// How long a run's browser may take to start and check the saved sign-ins.
const READY_WAIT: Duration = Duration::from_secs(2 * 60);
/// How long a sign-in window stays open without being saved.
const SIGN_IN_WAIT: Duration = Duration::from_secs(30 * 60);
/// How long opening the relay to the hub may take, and the wait before
/// trying again.
const RELAY_CONNECT: Duration = Duration::from_secs(15);
const RELAY_RETRY: Duration = Duration::from_secs(5);
/// The most one message from the hub may weigh: input and acknowledgements
/// are small.
const MAX_RELAY_MESSAGE: usize = 64 << 10;

/// Whether the owner wants to see the run's browser, as the hub last said.
pub(crate) type Wants = watch::Sender<Option<BrowserWant>>;

/// Passes on what the hub's latest check said, if it changed.
pub(crate) fn want(w: &Wants, now: Option<BrowserWant>) {
    w.send_if_modified(|cur| {
        let changed = *cur != now;
        *cur = now;
        changed
    });
}

/// A saved sign-in stopped working: the browser box found it signed out in
/// a fresh browser loaded from the saved copy, not the agent's. This is
/// where the hub is to be told, so the owner gets a card asking them to
/// sign in again; until saved sign-ins reach runs it only notes it.
pub(crate) async fn report_signed_out(hub: &Hub, thread: i64, sign_in_id: i64) {
    tracing::info!(thread, sign_in_id, "a saved sign-in no longer works");
    // Told in the background: a slow hub must not hold up the browser.
    let hub = hub.clone();
    tokio::spawn(async move {
        if let Err(err) =
            decurion_runner::sign_ins::report_signed_out(&hub.access(), sign_in_id).await
        {
            tracing::warn!(%err, thread, sign_in_id, "could not tell the hub a sign-in stopped working");
        }
    });
}

/// What the owner signed in to in a sign-in window, to be saved. Not
/// `Debug`: it holds the sign-in itself.
pub(crate) struct Captured {
    /// Playwright's storage state: cookies and localStorage.
    pub state: serde_json::Value,
    /// The page where the password was typed, if one was.
    pub login_url: Option<String>,
    /// The sites the sign-in covers.
    pub domains: Vec<String>,
    /// When its first lasting cookie ends (Unix seconds).
    pub expires_at: Option<i64>,
}

/// Removes a browser box when dropped, so no way out of a run or a sign-in
/// leaves one behind.
struct Remover {
    id: i64,
    name: String,
    armed: bool,
}

impl Remover {
    async fn remove(mut self) {
        self.armed = false;
        if !remove_named(self.id, std::slice::from_ref(&self.name)).await {
            tracing::error!(container = %self.name, "could not remove the browser box");
        }
    }
}

impl Drop for Remover {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let (id, name) = (self.id, std::mem::take(&mut self.name));
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                if !remove_named(id, std::slice::from_ref(&name)).await {
                    tracing::error!(container = %name, "could not remove the browser box");
                }
            });
        }
    }
}

/// The browser box's pipes: lines to it, checked messages from it.
struct BoxIo {
    to_box: mpsc::Sender<String>,
    from_box: mpsc::Receiver<FromBrowser>,
    stderr: JoinHandle<String>,
    // Kept so the docker client lives as long as the box is used.
    _child: Option<Child>,
}

impl BoxIo {
    /// Sends a line without waiting: one the box is too busy to take is
    /// dropped, like a frame nobody can show.
    fn send(&self, msg: ToBrowser) {
        match encode(&msg) {
            Ok(line) => {
                if self.to_box.try_send(line).is_err() {
                    tracing::debug!("browser box busy; a line to it was dropped");
                }
            }
            Err(err) => tracing::warn!(%err, "line for the browser box refused"),
        }
    }
}

/// Starts a browser box and sends its first line. `secrets` are hidden from
/// what it prints on stderr.
async fn spawn_box(
    args: Vec<String>,
    first: &ToBrowser,
    secrets: Vec<String>,
) -> anyhow::Result<BoxIo> {
    let first = encode(first)?;
    let mut child = docker()
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("could not start docker")?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let stdout = child.stdout.take().context("no stdout")?;
    let stderr = spawn_stderr_tail(child.stderr.take().context("no stderr")?, secrets);
    let (to_box, mut lines) = mpsc::channel::<String>(64);
    tokio::spawn(async move {
        while let Some(line) = lines.recv().await {
            if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
        // Dropping stdin closes it, and the box ends.
    });
    // The first line holds the saved sign-ins: waited for, never dropped.
    to_box
        .send(first)
        .await
        .context("the browser box closed at once")?;
    let (tx, from_box) = mpsc::channel(16);
    tokio::spawn(async move {
        let mut r = BufReader::new(stdout);
        while let Ok(Some((line, cut))) = read_line_up_to(&mut r, MAX_FROM_LINE).await {
            if cut {
                tracing::warn!("over-long line from the browser box; ignored");
                continue;
            }
            match decode(&line) {
                Ok(msg) => {
                    if tx.send(msg).await.is_err() {
                        break;
                    }
                }
                Err(err) => tracing::warn!(%err, "odd line from the browser box; ignored"),
            }
        }
    });
    Ok(BoxIo {
        to_box,
        from_box,
        stderr,
        _child: Some(child),
    })
}

/// A work run's browser box, up and checked. Dropping it removes the box.
pub(crate) struct RunBrowser {
    wants: Wants,
    remover: Remover,
    task: JoinHandle<()>,
}

impl RunBrowser {
    pub(crate) fn wants(&self) -> &Wants {
        &self.wants
    }
}

/// Starts a work run's browser box with the app's saved sign-ins and waits
/// until it is ready: up, the tools served, each sign-in checked. Sign-ins
/// found signed out are reported, and the run goes ahead without them.
pub(crate) async fn start_run(
    hub: &Hub,
    thread: i64,
    states: Vec<SiteState>,
    secrets: &[String],
) -> anyhow::Result<RunBrowser> {
    let remover = Remover {
        id: thread,
        name: browser_container_name(thread),
        armed: true,
    };
    let mut io = spawn_box(
        browser_box_args(thread),
        &start_line(states),
        secrets.to_vec(),
    )
    .await?;
    let deadline = Instant::now() + READY_WAIT;
    loop {
        let msg = tokio::select! {
            m = io.from_box.recv() => m,
            _ = tokio::time::sleep_until(deadline) => bail!(
                "The test browser did not get ready within two minutes, so the run did not start. \
        Try again; if it keeps happening, rebuild the worker image."
            ),
        };
        match msg {
            Some(FromBrowser::Ready { .. }) => break,
            Some(FromBrowser::SignedOut { sign_in_id }) => {
                report_signed_out(hub, thread, sign_in_id).await
            }
            Some(FromBrowser::Failed { why }) => {
                bail!("The test browser could not start, so the run did not start: {why}")
            }
            Some(_) => {}
            None => {
                let tail = tokio::time::timeout(Duration::from_secs(5), &mut io.stderr)
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or_default();
                tracing::warn!(thread, stderr = %tail.trim(), "browser box ended before it was ready");
                bail!(
                    "The test browser stopped before it was ready, so the run did not start. \
Make sure Docker is running and the worker image is up to date."
                )
            }
        }
    }
    let (wants, wanted) = watch::channel(None);
    let hub = hub.clone();
    let task = tokio::spawn(async move {
        let (end, relay) = relay_loop(&hub, Some(thread), BrowserMode::Run, io, wanted).await;
        finish(relay, &ended_why(&end)).await;
    });
    Ok(RunBrowser {
        wants,
        remover,
        task,
    })
}

/// Removes a run's browser box once the agent is done with it, and lets the
/// relay tell the hub.
pub(crate) async fn stop(browser: Option<RunBrowser>) {
    let Some(b) = browser else { return };
    b.remover.remove().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), b.task).await;
}

/// Opens a sign-in window: a fresh browser at `start_url`, shown to the
/// owner through browser session `session_id` and driven by them alone,
/// with no agent and no tools. Returns what to save once the owner taps
/// "I'm signed in, save". The password is never kept: only what the browser
/// holds afterwards. The owner's screen stays connected until the caller
/// says how saving went ([`SignInWindow::finish`]): the hub only takes the
/// save while the window is still open.
pub(crate) async fn run_sign_in_box(
    hub: &Hub,
    session_id: i64,
    start_url: &str,
) -> anyhow::Result<(Captured, SignInWindow)> {
    let first = start_sign_in(start_url)?;
    let name = sign_in_container_name(session_id);
    // One left by a runner that died.
    if !remove_named(session_id, std::slice::from_ref(&name)).await {
        bail!("could not remove the Docker container {name} left from an earlier sign-in");
    }
    let remover = Remover {
        id: session_id,
        name,
        armed: true,
    };
    let io = spawn_box(sign_in_box_args(session_id), &first, Vec::new()).await?;
    let (_wants, wanted) = watch::channel(Some(BrowserWant { session_id }));
    let end = tokio::time::timeout(
        SIGN_IN_WAIT,
        relay_loop(hub, None, BrowserMode::SignIn, io, wanted),
    )
    .await;
    remover.remove().await;
    let (end, relay) = match end {
        Ok(r) => r,
        Err(_) => bail!(
            "The sign-in window was open for 30 minutes without being saved, so it was closed."
        ),
    };
    match end {
        BoxEnd::Captured(c) => Ok((c, SignInWindow { relay })),
        BoxEnd::Failed(why) => {
            finish(relay, &why).await;
            bail!("The sign-in window stopped: {why}")
        }
        BoxEnd::Gone => {
            finish(relay, &ended_why(&end)).await;
            bail!("The sign-in window closed before the sign-in was saved.")
        }
    }
}

/// What the owner's screen is told once a sign-in has been saved. The
/// screen checks for exactly this.
pub(crate) const SIGN_IN_SAVED: &str = "The sign-in was saved.";

/// A sign-in window whose browser is gone but whose screen is still
/// connected, waiting to hear whether the sign-in was saved.
pub(crate) struct SignInWindow {
    relay: Option<Relay>,
}

impl SignInWindow {
    /// Tells the owner's screen how saving went, and closes it.
    pub(crate) async fn finish(self, why: &str) {
        finish(self.relay, why).await;
    }
}

/// Why the owner's screen closes, when nobody else says.
fn ended_why(end: &BoxEnd) -> String {
    match end {
        BoxEnd::Captured(_) => SIGN_IN_SAVED.to_owned(),
        BoxEnd::Failed(why) => why.clone(),
        BoxEnd::Gone => "The browser has closed.".to_owned(),
    }
}

/// Ends the owner's view, saying why, and closes the relay.
async fn finish(relay: Option<Relay>, why: &str) {
    if let Some(r) = relay {
        let _ =
            r.up.send(RelayUp::Ended {
                why: why.to_owned(),
            })
            .await;
        drop(r.up);
        let _ = tokio::time::timeout(Duration::from_secs(5), r.task).await;
    }
}

/// How a browser box's lines ended.
enum BoxEnd {
    /// Sign-in mode: what to save.
    Captured(Captured),
    Failed(String),
    /// The box is gone (the run ended, or it was removed).
    Gone,
}

/// The relay to the hub for one browser session.
struct Relay {
    session: i64,
    up: mpsc::Sender<RelayUp>,
    down: mpsc::Receiver<RelayDown>,
    task: JoinHandle<()>,
}

/// Opens the relay: an outbound WebSocket to the hub with the runner's own
/// key, so nothing ever connects in to the runner.
async fn open_relay(hub: &Hub, session: i64) -> anyhow::Result<Relay> {
    let mut req = relay_url(&hub.base, session)?.into_client_request()?;
    req.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", hub.key))?,
    );
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_RELAY_MESSAGE))
        .max_frame_size(Some(MAX_RELAY_MESSAGE));
    let (ws, _) = tokio::time::timeout(
        RELAY_CONNECT,
        tokio_tungstenite::connect_async_with_config(req, Some(config), false),
    )
    .await
    .context("the hub did not answer")??;
    let (mut sink, mut stream) = ws.split();
    // Room for a frame and a page or two: a frame that does not fit is
    // dropped (see relay_loop), never queued.
    let (up, mut up_rx) = mpsc::channel::<RelayUp>(4);
    let (down_tx, down) = mpsc::channel::<RelayDown>(32);
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                m = up_rx.recv() => {
                    let Some(m) = m else {
                        let _ = sink.send(Message::Close(None)).await;
                        break;
                    };
                    let Ok(text) = serde_json::to_string(&m) else { continue };
                    if sink.send(Message::text(text)).await.is_err() {
                        break;
                    }
                }
                m = stream.next() => match m {
                    Some(Ok(Message::Text(t))) => match serde_json::from_str::<RelayDown>(t.as_str()) {
                        Ok(d) => {
                            if down_tx.send(d).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => tracing::debug!("odd message from the hub's relay; ignored"),
                    },
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
            }
        }
    });
    Ok(Relay {
        session,
        up,
        down,
        task,
    })
}

/// The next message from the hub, or never if there is no relay.
async fn next_down(relay: &mut Option<Relay>) -> Option<RelayDown> {
    match relay {
        Some(r) => r.down.recv().await,
        None => std::future::pending().await,
    }
}

/// Nobody is watching any more: stop the screen, and in a run give the
/// browser back to the agent, so it never waits on an owner who has gone.
fn unwatched(io: &BoxIo, mode: BrowserMode) {
    io.send(ToBrowser::Screencast {
        on: false,
        max_width: MIN_WIDTH,
        quality: QUALITY,
    });
    if mode == BrowserMode::Run {
        io.send(ToBrowser::Control {
            who: Controller::Agent,
        });
    }
}

/// Follows a browser box until it ends: reports saved sign-ins it finds
/// signed out, and while the hub wants the browser shown (`wanted`), keeps
/// a relay open and passes the screen up and the owner's input down.
/// Returns the relay still open, for the caller to [`finish`].
async fn relay_loop(
    hub: &Hub,
    thread: Option<i64>,
    mode: BrowserMode,
    mut io: BoxIo,
    mut wanted: watch::Receiver<Option<BrowserWant>>,
) -> (BoxEnd, Option<Relay>) {
    let mut relay: Option<Relay> = None;
    let mut page: Option<RelayUp> = None;
    let mut wants_open = true;
    let mut retry = wanted.borrow_and_update().is_some().then(Instant::now);
    let end = loop {
        tokio::select! {
            msg = io.from_box.recv() => match msg {
                None => break BoxEnd::Gone,
                Some(FromBrowser::Failed { why }) => break BoxEnd::Failed(why),
                Some(FromBrowser::Captured { state, login_url, domains, expires_at }) => {
                    if mode == BrowserMode::SignIn {
                        break BoxEnd::Captured(Captured { state, login_url, domains, expires_at });
                    }
                }
                Some(FromBrowser::SignedOut { sign_in_id }) => {
                    if let Some(t) = thread {
                        report_signed_out(hub, t, sign_in_id).await;
                    }
                }
                Some(f @ FromBrowser::Frame { .. }) => {
                    let FromBrowser::Frame { seq, .. } = f else { continue };
                    let sent = match (&relay, to_relay(&f)) {
                        (Some(r), Some(up)) => r.up.try_send(up).is_ok(),
                        _ => false,
                    };
                    if !sent {
                        // Dropped, not queued: free the box for a newer one.
                        io.send(ToBrowser::FrameAck { seq });
                    }
                }
                Some(f @ FromBrowser::Page { .. }) => {
                    page = to_relay(&f);
                    if let (Some(r), Some(up)) = (&relay, page.clone()) {
                        let _ = r.up.try_send(up);
                    }
                }
                Some(FromBrowser::Ready { .. }) => {}
            },
            changed = wanted.changed(), if wants_open => {
                // The run is over when the sender goes: nothing more is wanted.
                wants_open = changed.is_ok();
                let want = if wants_open { wanted.borrow_and_update().clone() } else { None };
                let same = matches!((&want, &relay), (Some(w), Some(r)) if r.session == w.session_id);
                if !same {
                    if relay.take().is_some() {
                        unwatched(&io, mode);
                    }
                    retry = want.is_some().then(Instant::now);
                }
            },
            d = next_down(&mut relay) => match d {
                Some(d) => {
                    if let Some(line) = from_relay(d, mode) {
                        io.send(line);
                    }
                }
                None => {
                    // The hub closed the relay; open it again if still wanted.
                    relay = None;
                    unwatched(&io, mode);
                    if wanted.borrow().is_some() {
                        retry = Some(Instant::now() + RELAY_RETRY);
                    }
                }
            },
            _ = tokio::time::sleep_until(retry.unwrap_or_else(Instant::now)), if retry.is_some() => {
                retry = None;
                let want = wanted.borrow().clone();
                if let Some(w) = want {
                    match open_relay(hub, w.session_id).await {
                        Ok(r) => {
                            // The viewer sees where the browser is at once.
                            if let Some(p) = page.clone() {
                                let _ = r.up.try_send(p);
                            }
                            relay = Some(r);
                        }
                        Err(err) => {
                            tracing::warn!(%err, session = w.session_id, "could not open the browser relay to the hub; trying again");
                            retry = Some(Instant::now() + RELAY_RETRY);
                        }
                    }
                }
            },
        }
    };
    (end, relay)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_change_in_what_the_hub_wants_wakes_the_relay() {
        let (tx, mut rx) = watch::channel(None);
        want(&tx, None);
        assert!(!rx.has_changed().unwrap_or(true));
        want(&tx, Some(BrowserWant { session_id: 4 }));
        assert!(rx.has_changed().unwrap_or(false));
        rx.borrow_and_update();
        want(&tx, Some(BrowserWant { session_id: 4 }));
        assert!(!rx.has_changed().unwrap_or(true));
        want(&tx, Some(BrowserWant { session_id: 5 }));
        assert_eq!(*rx.borrow_and_update(), Some(BrowserWant { session_id: 5 }));
    }

    use decurion_proto::browser::Input;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
    use tokio_tungstenite::WebSocketStream;

    type HubSide = WebSocketStream<tokio::net::TcpStream>;
    const WAIT: Duration = Duration::from_secs(10);

    /// A browser box made of channels: what the runner writes to it, and a
    /// way to make it print.
    fn fake_box() -> (BoxIo, mpsc::Receiver<String>, mpsc::Sender<FromBrowser>) {
        let (to_box, lines) = mpsc::channel(64);
        let (prints, from_box) = mpsc::channel(16);
        let io = BoxIo {
            to_box,
            from_box,
            stderr: tokio::spawn(async { String::new() }),
            _child: None,
        };
        (io, lines, prints)
    }

    fn hub_at(addr: std::net::SocketAddr) -> Hub {
        Hub {
            base: format!("http://{addr}"),
            key: "runner-key".into(),
            http: reqwest::Client::new(),
            name: "pc".into(),
            place: decurion_proto::Place::Pc,
        }
    }

    /// Accepts the runner's relay, returning the path and key it came with.
    // The handshake callback's error type is tungstenite's, not ours.
    #[allow(clippy::result_large_err)]
    async fn accept(l: &TcpListener) -> anyhow::Result<(HubSide, String, String)> {
        loop {
            let (tcp, _) = tokio::time::timeout(WAIT, l.accept()).await??;
            let (tx, rx) = tokio::sync::oneshot::channel();
            let ws = tokio_tungstenite::accept_hdr_async(tcp, |req: &Request, resp: Response| {
                let auth = req
                    .headers()
                    .get(AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_owned();
                let _ = tx.send((req.uri().path().to_owned(), auth));
                Ok(resp)
            })
            .await;
            // The runner's plain calls to the hub (a sign-in reported signed
            // out) reach the same address; only the relay is wanted here.
            let Ok(ws) = ws else { continue };
            let (path, auth) = rx.await?;
            return Ok((ws, path, auth));
        }
    }

    async fn up(ws: &mut HubSide) -> anyhow::Result<RelayUp> {
        loop {
            match tokio::time::timeout(WAIT, ws.next()).await? {
                Some(Ok(Message::Text(t))) => return Ok(serde_json::from_str(t.as_str())?),
                Some(Ok(_)) => continue,
                other => anyhow::bail!("relay ended: {other:?}"),
            }
        }
    }

    async fn down(ws: &mut HubSide, d: RelayDown) -> anyhow::Result<()> {
        ws.send(Message::text(serde_json::to_string(&d)?)).await?;
        Ok(())
    }

    async fn line(lines: &mut mpsc::Receiver<String>) -> anyhow::Result<ToBrowser> {
        let l = tokio::time::timeout(WAIT, lines.recv())
            .await?
            .context("box closed")?;
        Ok(serde_json::from_str(&l)?)
    }

    #[tokio::test]
    async fn a_watched_run_relays_the_screen_up_and_the_owner_down() -> anyhow::Result<()> {
        let l = TcpListener::bind("127.0.0.1:0").await?;
        let hub = hub_at(l.local_addr()?);
        let (io, mut lines, prints) = fake_box();
        let (wants, wanted) = watch::channel(Some(BrowserWant { session_id: 7 }));
        let looping = tokio::spawn(async move {
            let (end, relay) = relay_loop(&hub, Some(3), BrowserMode::Run, io, wanted).await;
            finish(relay, &ended_why(&end)).await;
            end
        });

        let (mut ws, path, auth) = accept(&l).await?;
        assert_eq!(path, "/api/runner/browser/7/ws");
        assert_eq!(auth, "Bearer runner-key");

        let page = FromBrowser::Page {
            url: "https://example.com/".into(),
            title: "Example".into(),
        };
        prints.send(page).await?;
        assert!(matches!(up(&mut ws).await?, RelayUp::Page { title, .. } if title == "Example"));
        prints
            .send(FromBrowser::Frame {
                seq: 1,
                jpeg: "/9j/".into(),
                width: 640,
                height: 400,
            })
            .await?;
        assert!(matches!(up(&mut ws).await?, RelayUp::Frame { seq: 1, .. }));
        // Saved sign-ins never go up; the runner reports them itself.
        prints
            .send(FromBrowser::SignedOut { sign_in_id: 9 })
            .await?;

        down(&mut ws, RelayDown::FrameAck { seq: 1 }).await?;
        assert_eq!(line(&mut lines).await?, ToBrowser::FrameAck { seq: 1 });
        let click = Input::Click { x: 0.5, y: 0.5 };
        down(
            &mut ws,
            RelayDown::Input {
                input: click.clone(),
            },
        )
        .await?;
        assert_eq!(line(&mut lines).await?, ToBrowser::Input { input: click });
        // Saving and odd input are not passed on during a run.
        down(&mut ws, RelayDown::Capture).await?;
        let odd = Input::Go {
            url: "file:///etc/passwd".into(),
        };
        down(&mut ws, RelayDown::Input { input: odd }).await?;
        down(
            &mut ws,
            RelayDown::Control {
                who: Controller::Owner,
            },
        )
        .await?;
        assert_eq!(
            line(&mut lines).await?,
            ToBrowser::Control {
                who: Controller::Owner
            }
        );

        // The viewer goes: the screen stops and the agent gets the browser
        // back. A frame with no one to see it is let go at once.
        ws.close(None).await?;
        assert!(matches!(
            line(&mut lines).await?,
            ToBrowser::Screencast { on: false, .. }
        ));
        assert_eq!(
            line(&mut lines).await?,
            ToBrowser::Control {
                who: Controller::Agent
            }
        );
        prints
            .send(FromBrowser::Frame {
                seq: 2,
                jpeg: "/9j/".into(),
                width: 640,
                height: 400,
            })
            .await?;
        assert_eq!(line(&mut lines).await?, ToBrowser::FrameAck { seq: 2 });

        // Still wanted, so the relay comes back, and hears when the browser
        // ends.
        let (mut ws, _, _) = accept(&l).await?;
        assert!(matches!(up(&mut ws).await?, RelayUp::Page { .. }));
        drop(prints);
        assert!(matches!(up(&mut ws).await?, RelayUp::Ended { .. }));
        let end = tokio::time::timeout(WAIT, looping).await??;
        assert!(matches!(end, BoxEnd::Gone));
        drop(wants);
        Ok(())
    }

    #[tokio::test]
    async fn a_sign_in_window_hands_back_what_to_save() -> anyhow::Result<()> {
        let l = TcpListener::bind("127.0.0.1:0").await?;
        let hub = hub_at(l.local_addr()?);
        let (io, mut lines, prints) = fake_box();
        let (_wants, wanted) = watch::channel(Some(BrowserWant { session_id: 8 }));
        let looping =
            tokio::spawn(
                async move { relay_loop(&hub, None, BrowserMode::SignIn, io, wanted).await },
            );
        let (mut ws, path, _) = accept(&l).await?;
        assert_eq!(path, "/api/runner/browser/8/ws");
        down(&mut ws, RelayDown::Capture).await?;
        assert_eq!(line(&mut lines).await?, ToBrowser::Capture);
        prints
            .send(FromBrowser::Captured {
                state: serde_json::json!({"cookies": [], "origins": []}),
                login_url: Some("https://example.com/login".into()),
                domains: vec!["example.com".into()],
                expires_at: Some(1_800_000_000),
            })
            .await?;
        // The screen hears nothing until the caller says how saving went:
        // the hub only takes the save while the window is open.
        let (end, relay) = tokio::time::timeout(WAIT, looping).await??;
        assert!(matches!(end, BoxEnd::Captured(c) if c.domains == ["example.com"]));
        assert!(relay.is_some());
        let told = tokio::spawn(async move { SignInWindow { relay }.finish(SIGN_IN_SAVED).await });
        assert!(matches!(up(&mut ws).await?, RelayUp::Ended { why } if why == SIGN_IN_SAVED));
        told.await?;
        Ok(())
    }
}
