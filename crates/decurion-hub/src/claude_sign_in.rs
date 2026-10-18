//! "Sign in on this PC" for a Max plan. The hub runs Claude Code's own
//! `claude setup-token` on the PC, inside a pseudo-terminal (it shows nothing
//! without one), and picks the new sign-in out of what it prints. Claude opens
//! its sign-in page in the PC's browser; once the owner signs in there, the
//! sign-in goes straight into the vault, under the name the runner reads. If
//! the page did not open, the board shows Claude's own link and a box for the
//! code that page gives, which goes to Claude as if typed.
//!
//! What must stay true:
//! - The sign-in is held only in memory here, then saved in the vault. It is
//!   never logged, never sent to a page, never put in the database (only the
//!   start of its SHA-256, to tell whether the saved one is still the one
//!   dated here), and never handed to a box from here.
//! - Nothing Claude prints is logged or shown: it may hold the sign-in.
//! - Only the official Claude Code CLI makes the sign-in; the hub never talks
//!   to Claude's sign-in service itself.
//! - One sign-in at a time, stopped after [`GIVE_UP`] so a forgotten one does
//!   not run forever. Claude gets only fixed arguments; the owner's code is
//!   one line of plain characters.

use crate::store::{Result, Store};
use crate::{forbidden, no_vault, ApiError, AppState, Caller};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use decurion_proto::Profile;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, Notify};

/// How long the owner has to finish signing in.
const GIVE_UP: Duration = Duration::from_secs(10 * 60);
/// What Claude prints is kept up to this much, newest last.
const KEEP_BYTES: usize = 256 * 1024;
/// Wide enough that Claude never wraps its sign-in or its link.
const COLUMNS: u16 = 1000;

/// Where the owner is in signing in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Waiting,
    Saved,
    Failed,
}

/// What the board shows. Never holds the sign-in itself.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub phase: Phase,
    pub profile: Option<Profile>,
    /// Claude's own sign-in link, for when its page did not open by itself.
    pub link: Option<String>,
    /// Claude is ready for the code its page shows.
    pub wants_code: bool,
    pub message: Option<String>,
}

impl Status {
    fn idle() -> Self {
        Self {
            phase: Phase::Idle,
            profile: None,
            link: None,
            wants_code: false,
            message: None,
        }
    }
}

struct Inner {
    /// Bumped by each start, so a finished sign-in never overwrites a newer one.
    seq: u64,
    status: Status,
    /// Typing into Claude's window, while one is open.
    writer: Option<Box<dyn Write + Send>>,
    stop: Option<Arc<Notify>>,
}

/// The sign-in going on now, if any, shared by the routes and the task that
/// watches Claude.
#[derive(Clone)]
pub struct SignIns {
    inner: Arc<Mutex<Inner>>,
    /// Set by tests; otherwise Claude Code is looked for on each start, so
    /// installing it while Decurion runs is enough.
    program: Option<Arc<PathBuf>>,
}

impl Default for SignIns {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                seq: 0,
                status: Status::idle(),
                writer: None,
                stop: None,
            })),
            program: None,
        }
    }
}

impl SignIns {
    /// Runs `program` instead of Claude Code (for tests).
    pub fn with_program(program: PathBuf) -> Self {
        Self {
            program: Some(Arc::new(program)),
            ..Self::default()
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panic while holding this lock leaves plain data behind, so it is
        // still safe to use.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self) -> Status {
        self.lock().status.clone()
    }

    fn program(&self) -> Option<PathBuf> {
        match &self.program {
            Some(p) => Some(p.as_ref().clone()),
            None => find_claude(),
        }
    }

    /// Starts a new sign-in, unless one is already open.
    fn begin(&self, profile: Profile) -> std::result::Result<(u64, Arc<Notify>), &'static str> {
        let mut g = self.lock();
        if g.status.phase == Phase::Waiting {
            return Err("A Claude sign-in is already open. Finish it, or tap Cancel first.");
        }
        g.seq += 1;
        let stop = Arc::new(Notify::new());
        g.stop = Some(stop.clone());
        g.writer = None;
        g.status = Status {
            phase: Phase::Waiting,
            profile: Some(profile),
            link: None,
            wants_code: false,
            message: None,
        };
        Ok((g.seq, stop))
    }

    fn set_writer(&self, seq: u64, w: Box<dyn Write + Send>) {
        let mut g = self.lock();
        if g.seq == seq {
            g.writer = Some(w);
        }
    }

    fn progress(&self, seq: u64, link: Option<String>, wants_code: bool) {
        let mut g = self.lock();
        if g.seq == seq && g.status.phase == Phase::Waiting {
            g.status.link = link;
            g.status.wants_code = wants_code;
        }
    }

    fn finish(&self, seq: u64, phase: Phase, message: Option<String>) {
        let mut g = self.lock();
        if g.seq != seq {
            return;
        }
        g.writer = None;
        g.stop = None;
        let profile = g.status.profile;
        g.status = Status {
            phase,
            profile: if phase == Phase::Idle { None } else { profile },
            link: None,
            wants_code: false,
            message,
        };
    }
}

/// Claude Code on this PC: on the PATH, or where its own installer and npm
/// put it, which a hub started from the Start menu may not have on its PATH.
fn find_claude() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["claude.exe", "claude.cmd"]
    } else {
        &["claude"]
    };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let home = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    if let Some(h) = std::env::var_os(home) {
        dirs.push(PathBuf::from(h).join(".local").join("bin"));
    }
    if cfg!(windows) {
        if let Some(a) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(a).join("npm"));
        }
    } else {
        dirs.push("/opt/homebrew/bin".into());
        dirs.push("/usr/local/bin".into());
    }
    dirs.iter()
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|p| p.is_file())
}

/// What Claude has printed so far, as plain text without terminal codes,
/// plus the targets of the links it drew.
#[derive(Default)]
struct Screen {
    raw: Vec<u8>,
}

impl Screen {
    fn push(&mut self, bytes: &[u8]) {
        self.raw.extend_from_slice(bytes);
        if self.raw.len() > KEEP_BYTES {
            let cut = self.raw.len() - KEEP_BYTES;
            self.raw.drain(..cut);
        }
    }

    fn read(&self) -> (String, Vec<String>) {
        plain(&String::from_utf8_lossy(&self.raw))
    }
}

/// Terminal output as plain text. Colour codes are dropped; codes that move
/// the cursor become a space, so words they separate stay apart. Link codes
/// (OSC 8) are dropped from the text and their targets returned.
fn plain(s: &str) -> (String, Vec<String>) {
    let mut out = String::with_capacity(s.len());
    let mut links = Vec::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\u{1b}' => match it.next() {
                Some('[') => {
                    let mut last = 'm';
                    for c in it.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            last = c;
                            break;
                        }
                    }
                    if last != 'm' {
                        out.push(' ');
                    }
                }
                Some(']') => {
                    let mut body = String::new();
                    while let Some(c) = it.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' {
                            it.next_if_eq(&'\\');
                            break;
                        }
                        body.push(c);
                    }
                    if let Some(rest) = body.strip_prefix("8;") {
                        if let Some((_, target)) = rest.split_once(';') {
                            if !target.is_empty() && !links.iter().any(|l| l == target) {
                                links.push(target.to_owned());
                            }
                        }
                    }
                }
                _ => {}
            },
            '\r' | '\n' => out.push('\n'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    (out, links)
}

/// The sign-in Claude printed, once it is printed whole (something follows
/// it), and only if it has the shape of one.
fn find_token(text: &str) -> Option<String> {
    let start = text.find("sk-ant-oat")?;
    let rest = &text[start..];
    let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))?;
    let token = &rest[..end];
    decurion_vault::check_claude_token(token).ok()?;
    Some(token.to_owned())
}

/// Claude's link for signing in by hand, if it showed one.
fn find_link(links: &[String]) -> Option<String> {
    links
        .iter()
        .find(|l| l.starts_with("https://") && l.contains("/oauth/authorize"))
        .cloned()
}

fn fingerprint(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// When a Max plan's sign-in was saved from the board, and when it runs out.
#[derive(Debug, Clone, Serialize)]
pub struct Dated {
    pub saved_at: String,
    pub expires_at: String,
}

impl Store {
    /// Dates a sign-in saved just now: Claude's sign-ins last one year.
    async fn date_claude_sign_in(&self, p: Profile, fingerprint: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO claude_sign_ins (profile, fingerprint, saved_at, expires_at)
             VALUES (?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                     strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+365 days'))
             ON CONFLICT (profile) DO UPDATE SET fingerprint = excluded.fingerprint,
                 saved_at = excluded.saved_at, expires_at = excluded.expires_at",
        )
        .bind(p.as_str())
        .bind(fingerprint)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    async fn claude_sign_in_date(&self, p: Profile) -> Result<Option<(String, Dated)>> {
        let row = sqlx::query(
            "SELECT fingerprint, saved_at, expires_at FROM claude_sign_ins WHERE profile = ?",
        )
        .bind(p.as_str())
        .fetch_optional(self.pool())
        .await?;
        row.map(|r| {
            Ok((
                r.try_get("fingerprint")?,
                Dated {
                    saved_at: r.try_get("saved_at")?,
                    expires_at: r.try_get("expires_at")?,
                },
            ))
        })
        .transpose()
    }
}

/// Whether a Max plan has a sign-in saved on this PC, and its dates when
/// Decurion saved it. `None` when there is no vault to look in.
pub(crate) async fn saved(st: &AppState, p: Profile) -> Result<Option<(bool, Option<Dated>)>> {
    let Some(vault) = st.vault.clone() else {
        return Ok(None);
    };
    let token = tokio::task::spawn_blocking(move || vault.get_claude(p).ok().flatten())
        .await
        .ok()
        .flatten();
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        return Ok(Some((false, None)));
    };
    let fp = fingerprint(&token);
    drop(token);
    let dated = st
        .store
        .claude_sign_in_date(p)
        .await?
        .and_then(|(f, d)| (f == fp).then_some(d));
    Ok(Some((true, dated)))
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/claude/sign-in", get(status).post(start))
        .route("/claude/sign-in/code", post(code))
        .route("/claude/sign-in/cancel", post(cancel))
}

fn refused(text: &str) -> Response {
    (StatusCode::CONFLICT, text.to_owned()).into_response()
}

async fn status(State(st): State<AppState>, Extension(c): Extension<Caller>) -> Response {
    if c != Caller::Owner {
        return forbidden("signing in to Claude");
    }
    Json(st.claude.status()).into_response()
}

#[derive(Deserialize)]
struct Start {
    profile: Profile,
}

/// The owner's "Sign in on this PC" tap.
async fn start(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Json(b): Json<Start>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("signing in to Claude");
    }
    let Some(vault) = st.vault.clone() else {
        return no_vault();
    };
    let Some(program) = st.claude.program() else {
        return refused(
            "Claude Code is not installed on this PC. Install it, then tap Sign in on this PC again.",
        );
    };
    let (seq, stop) = match st.claude.begin(b.profile) {
        Ok(v) => v,
        Err(text) => return refused(text),
    };
    match open(&program) {
        Ok((master, child)) => {
            match master.take_writer() {
                Ok(w) => st.claude.set_writer(seq, w),
                Err(e) => tracing::warn!(error = %e, "could not type into Claude's sign-in"),
            }
            tokio::spawn(watch(st.clone(), seq, b.profile, vault, master, child, stop));
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not start Claude Code for a sign-in");
            st.claude.finish(
                seq,
                Phase::Failed,
                Some(
                    "Couldn't start Claude Code on this PC. Check that it is installed, then try again."
                        .into(),
                ),
            );
        }
    }
    Json(st.claude.status()).into_response()
}

type Opened = (
    Box<dyn portable_pty::MasterPty + Send>,
    Box<dyn portable_pty::Child + Send + Sync>,
);

/// Starts `claude setup-token` in a pseudo-terminal of its own.
fn open(program: &std::path::Path) -> anyhow::Result<Opened> {
    use portable_pty::{CommandBuilder, PtySize};
    let pair = portable_pty::native_pty_system().openpty(PtySize {
        rows: 40,
        cols: COLUMNS,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(program);
    cmd.arg("setup-token");
    cmd.cwd(std::env::temp_dir());
    if cfg!(not(windows)) && std::env::var_os("TERM").is_none() {
        cmd.env("TERM", "xterm-256color");
    }
    let child = pair.slave.spawn_command(cmd)?;
    // Claude holds its own end now; keeping ours would hide when it closes.
    drop(pair.slave);
    Ok((pair.master, child))
}

enum Event {
    Out(Vec<u8>),
    Exited,
    Closed,
}

const STOPPED: &str = "Claude stopped before the sign-in finished. Try again.";
const CANCELLED: &str = "";

/// Watches Claude until it prints the sign-in, stops, is cancelled or runs
/// out of time, then saves the sign-in and ends Claude.
async fn watch(
    st: AppState,
    seq: u64,
    profile: Profile,
    vault: decurion_vault::Vault,
    master: Box<dyn portable_pty::MasterPty + Send>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    stop: Arc<Notify>,
) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut killer = child.clone_killer();
    match master.try_clone_reader() {
        Ok(mut reader) => {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx.send(Event::Out(buf[..n].to_vec())).is_err() {
                                return;
                            }
                        }
                    }
                }
                let _ = tx.send(Event::Closed);
            });
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not read Claude's sign-in window");
            let _ = tx.send(Event::Closed);
        }
    }
    std::thread::spawn(move || {
        let _ = child.wait();
        let _ = tx.send(Event::Exited);
    });

    let mut screen = Screen::default();
    let give_up = tokio::time::sleep(GIVE_UP);
    tokio::pin!(give_up);
    // After Claude exits, what it printed last may still be on its way.
    let mut grace: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;
    let end: std::result::Result<String, &'static str> = loop {
        tokio::select! {
            _ = &mut give_up => break Err("The sign-in took too long, so Decurion stopped it. Try again."),
            _ = stop.notified() => break Err(CANCELLED),
            _ = async {
                match grace.as_mut() {
                    Some(g) => g.await,
                    None => std::future::pending().await,
                }
            } => break Err(STOPPED),
            ev = rx.recv() => match ev {
                Some(Event::Out(b)) => {
                    screen.push(&b);
                    let (text, links) = screen.read();
                    if let Some(t) = find_token(&text) {
                        break Ok(t);
                    }
                    st.claude.progress(seq, find_link(&links), text.contains("Paste code here"));
                }
                Some(Event::Exited) => {
                    if grace.is_none() {
                        grace = Some(Box::pin(tokio::time::sleep(Duration::from_secs(1))));
                    }
                }
                Some(Event::Closed) | None => break Err(STOPPED),
            },
        }
    };
    let _ = killer.kill();
    drop(master);
    drop(screen);
    match end {
        Ok(token) => {
            let fp = fingerprint(&token);
            let saved = tokio::task::spawn_blocking(move || vault.put_claude(profile, &token))
                .await
                .map_err(|_| ())
                .and_then(|r| r.map_err(|_| ()));
            if saved.is_err() {
                tracing::warn!(plan = profile.as_str(), "could not save a Claude sign-in");
                st.claude.finish(
                    seq,
                    Phase::Failed,
                    Some("Couldn't keep the sign-in on this PC. Try again.".into()),
                );
                return;
            }
            if let Err(e) = st.store.date_claude_sign_in(profile, &fp).await {
                tracing::warn!(error = %e, "could not date a Claude sign-in");
            }
            tracing::info!(plan = profile.as_str(), "saved a Claude sign-in from the board");
            st.claude.finish(
                seq,
                Phase::Saved,
                Some(format!(
                    "Signed in. Claude {} work uses it from its next piece of work.",
                    profile.as_str()
                )),
            );
        }
        Err(CANCELLED) => st.claude.finish(seq, Phase::Idle, None),
        Err(text) => st.claude.finish(seq, Phase::Failed, Some(text.into())),
    }
}

#[derive(Deserialize)]
struct Code {
    code: String,
}

/// The code Claude's page shows when it could not hand the sign-in back by
/// itself. It goes to Claude as one typed line.
async fn code(
    State(st): State<AppState>,
    Extension(c): Extension<Caller>,
    Json(b): Json<Code>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("signing in to Claude");
    }
    let code = b.code.trim();
    if code.is_empty()
        || code.len() > 512
        || !code.chars().all(|c| c.is_ascii_graphic())
    {
        return ApiError::from(crate::store::StoreError::Invalid(
            "Paste the code from Claude's page, without spaces.".into(),
        ))
        .into_response();
    }
    let mut g = st.claude.lock();
    if g.status.phase != Phase::Waiting {
        return refused("No Claude sign-in is waiting for a code. Tap Sign in on this PC first.");
    }
    let Some(w) = g.writer.as_mut() else {
        return refused("Claude is not ready for a code yet. Wait a moment and try again.");
    };
    if w.write_all(code.as_bytes())
        .and_then(|_| w.write_all(b"\r"))
        .and_then(|_| w.flush())
        .is_err()
    {
        return refused("Claude stopped listening. Tap Cancel and try again.");
    }
    g.status.wants_code = false;
    Json(g.status.clone()).into_response()
}

/// Stops a sign-in, or clears the last result from the page.
async fn cancel(State(st): State<AppState>, Extension(c): Extension<Caller>) -> Response {
    if c != Caller::Owner {
        return forbidden("signing in to Claude");
    }
    let stop = {
        let mut g = st.claude.lock();
        match g.status.phase {
            Phase::Waiting => g.stop.clone(),
            _ => {
                g.status = Status::idle();
                None
            }
        }
    };
    if let Some(s) = stop {
        s.notify_one();
    }
    Json(st.claude.status()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_codes_are_removed_and_links_kept() {
        let s = "\u{1b}[31mWelcome\u{1b}[9Gto\u{1b}[39m\r\n\u{1b}]8;id=1;https://claude.com/cai/oauth/authorize?code=true\u{7}\u{1b}[2mhttps://claude.com/cai\u{1b}[22m\u{1b}]8;;\u{7}\r\n";
        let (text, links) = plain(s);
        assert_eq!(text, "Welcome to\n\nhttps://claude.com/cai\n\n");
        assert_eq!(
            find_link(&links).as_deref(),
            Some("https://claude.com/cai/oauth/authorize?code=true")
        );
    }

    #[test]
    fn the_sign_in_is_found_only_once_printed_whole() {
        let t = format!("sk-ant-oat01-{}", "Ab_9-".repeat(20));
        assert_eq!(find_token(&format!("Your token:\n{t}\n")), Some(t.clone()));
        assert_eq!(find_token(&format!("Your token:\n{t}")), None);
        assert_eq!(find_token("Your token:\nsk-ant-oat01-short\n"), None);
        assert_eq!(find_token("sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789ABCDEF\n"), None);
    }
}
