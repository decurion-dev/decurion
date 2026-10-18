//! "Connect your phone": the hub sets up its own Tailscale https address, so
//! the owner never opens a terminal (`POST /api/phone/connect`).
//!
//! It asks `tailscale status --json` whether Tailscale is signed in and what
//! this PC is called on the owner's tailnet, runs `tailscale serve --bg
//! <port>` so Tailscale shares the hub at `https://NAME`, then saves that
//! address (`hub_settings`, migration 0076). Pairing codes and cloud workers
//! use it from then on, across restarts; `DECURION_PUBLIC_URL` still wins
//! when the hub starts. `GET /api/phone/connection` says which address the
//! hub uses now.
//!
//! Rules that must stay true:
//! - Owner only. A runner key gets 403, and `cloud_reach` lists neither
//!   route, so cloud workers never reach them.
//! - Only the owner's own tailnet, never the internet: it runs `serve`, and
//!   never `funnel`.
//! - Fixed arguments only. Nothing the owner types and nothing from a
//!   request goes on Tailscale's command line; the port is the hub's own.
//! - Nothing runs when the hub already has an https address.
//! - Never waits forever: each command has a time limit, its input is
//!   closed, and it is killed when the limit is reached or its answer is
//!   already known (`kill_on_drop`). `serve` waits for the owner when HTTPS
//!   is off for the tailnet, so its link is passed on as soon as it shows.
//! - Failures are plain words for the owner, with a link when there is one
//!   to open: Tailscale's download page, or the page Tailscale names to turn
//!   on HTTPS. A link taken from Tailscale's output is passed on only if it
//!   is https on login.tailscale.com.
//! - Short logs: Tailscale's output (no secrets in it, but long) goes to the
//!   log only when something failed, cut to a few hundred characters.
//! - One at a time: two taps at once run Tailscale once.

use crate::store::{Result as StoreResult, Store, StoreError};
use crate::{forbidden, ApiError, AppState, Caller};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Where the owner gets Tailscale.
pub const DOWNLOAD: &str = "https://tailscale.com/download";

/// Tailscale's DNS settings, where MagicDNS (which names each device) is
/// turned on.
pub const DNS_SETTINGS: &str = "https://login.tailscale.com/admin/dns";

/// The only site whose links from Tailscale's output are passed on.
const LINK_HOST: &str = "login.tailscale.com";

/// `tailscale serve` normally answers in a second or two.
const SERVE_SECS: u64 = 30;
/// `tailscale status` answers at once when Tailscale is running.
const STATUS_SECS: u64 = 15;

/// Where Tailscale's command-line program is on this computer: on the
/// `PATH`, else where its installer puts it (Windows, Mac).
pub fn find() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "tailscale.exe"
    } else {
        "tailscale"
    };
    if let Some(path) = std::env::var_os("PATH") {
        if let Some(p) = std::env::split_paths(&path)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
        {
            return Some(p);
        }
    }
    let mut known = Vec::new();
    if cfg!(windows) {
        if let Some(pf) = std::env::var_os("ProgramFiles") {
            known.push(PathBuf::from(pf).join("Tailscale").join("tailscale.exe"));
        }
        known.push(PathBuf::from(r"C:\Program Files\Tailscale\tailscale.exe"));
    } else if cfg!(target_os = "macos") {
        known.push(PathBuf::from(
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        ));
    }
    known.into_iter().find(|p| p.is_file())
}

/// Whether phones can get notifications through `url`: browsers allow them
/// only on https pages.
pub fn is_https(url: &str) -> bool {
    url.starts_with("https://")
}

/// Why "Connect your phone" did not work, in words the owner can act on.
#[derive(Debug)]
pub enum Failure {
    /// Tailscale's program is not on this computer.
    NotInstalled,
    /// Tailscale is installed but not running or not signed in.
    SignedOut,
    /// HTTPS is off for the tailnet; the link turns it on.
    HttpsOff(String),
    /// MagicDNS is off, so this PC has no name to use.
    NoName,
    /// Tailscale refused to change its settings for the hub (on Linux the
    /// user needs to be Tailscale's operator).
    NotAllowed,
    /// Tailscale did not answer in time.
    TooSlow,
    /// Anything else Tailscale said no to.
    Failed,
    /// The address could not be saved.
    Store(StoreError),
}

impl Failure {
    fn status(&self) -> StatusCode {
        match self {
            Failure::NotInstalled | Failure::TooSlow | Failure::Failed => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            _ => StatusCode::CONFLICT,
        }
    }

    /// The owner's words. Short, says what to do next.
    pub fn text(&self) -> &'static str {
        match self {
            Failure::NotInstalled => {
                "Tailscale is not on this PC. Install Tailscale on this PC and sign in, then try again."
            }
            Failure::SignedOut => {
                "Tailscale is not signed in on this PC. Open Tailscale on this PC and sign in, then try again."
            }
            Failure::HttpsOff(_) => {
                "Tailscale needs HTTPS turned on once. Open the link, turn it on, then tap Connect your phone again."
            }
            Failure::NoName => {
                "Tailscale has no name for this PC yet. Open the link, turn on MagicDNS, then try again."
            }
            Failure::NotAllowed => {
                "Tailscale did not let Decurion set it up. See Connect your phone in Help, then try again."
            }
            Failure::TooSlow => {
                "Tailscale took too long to answer. Check that Tailscale is on, then try again."
            }
            Failure::Failed | Failure::Store(_) => {
                "Tailscale could not share Decurion with your phone. Check that Tailscale is on and up to date, then try again."
            }
        }
    }

    /// A page for the owner to open, if one helps.
    pub fn link(&self) -> Option<&str> {
        match self {
            Failure::NotInstalled => Some(DOWNLOAD),
            Failure::HttpsOff(l) => Some(l),
            Failure::NoName => Some(DNS_SETTINGS),
            _ => None,
        }
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        if let Failure::Store(e) = self {
            return ApiError(e).into_response();
        }
        (
            self.status(),
            Json(json!({ "error": self.text(), "link": self.link() })),
        )
            .into_response()
    }
}

/// Makes the hub reachable from the owner's phones at its Tailscale https
/// address, saves that address and returns it. Returns the address the hub
/// has without running anything when it is https already.
pub async fn connect(st: &AppState) -> Result<Arc<str>, Failure> {
    let _one = st.connecting.lock().await;
    if let Some(u) = st.public_url().filter(|u| is_https(u)) {
        return Ok(u);
    }
    let program = st
        .tailscale
        .as_deref()
        .map(Path::to_path_buf)
        .or_else(find)
        .ok_or(Failure::NotInstalled)?;
    let name = status(&program).await?;
    serve(&program, st.port).await?;
    let url: Arc<str> = format!("https://{name}").into();
    st.store
        .save_public_url(&url)
        .await
        .map_err(Failure::Store)?;
    st.set_public_url(Some(url.clone()));
    tracing::info!(%url, "phones now reach the hub through Tailscale");
    Ok(url)
}

/// Starting the program failed: not there at all, or it would not start.
fn not_started(e: std::io::Error) -> Failure {
    if e.kind() == std::io::ErrorKind::NotFound {
        return Failure::NotInstalled;
    }
    tracing::warn!(error = %e, "could not start Tailscale");
    Failure::Failed
}

/// At most a few hundred characters of Tailscale's output, for the log.
fn short(s: &str) -> &str {
    let s = s.trim();
    match s.char_indices().nth(300) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// This PC's name on the tailnet, if Tailscale is running and signed in.
async fn status(program: &Path) -> Result<String, Failure> {
    let mut cmd = Command::new(program);
    cmd.args(["status", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = match tokio::time::timeout(Duration::from_secs(STATUS_SECS), cmd.output()).await {
        Err(_) => return Err(Failure::TooSlow),
        Ok(Err(e)) => return Err(not_started(e)),
        Ok(Ok(o)) => o,
    };
    // It prints its state even when signed out; with no answer at all,
    // Tailscale itself is not running.
    match serde_json::from_slice::<Value>(&out.stdout) {
        Ok(v) => read_status(&v),
        Err(_) => {
            let err = String::from_utf8_lossy(&out.stderr);
            tracing::warn!(output = %short(&err), "Tailscale gave no status");
            Err(Failure::SignedOut)
        }
    }
}

/// Reads `tailscale status --json`: signed in, and this PC's name without
/// the trailing dot.
pub fn read_status(v: &Value) -> Result<String, Failure> {
    if v["BackendState"].as_str() != Some("Running") {
        return Err(Failure::SignedOut);
    }
    let name = v["Self"]["DNSName"]
        .as_str()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let ok = name.contains('.')
        && name.len() <= 253
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    if ok {
        Ok(name)
    } else {
        Err(Failure::NoName)
    }
}

/// The link in one line of Tailscale's output that turns on HTTPS (or
/// another tailnet feature), if the line has one. Only https links on
/// login.tailscale.com, without a user name or password in them.
pub fn enable_link(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let raw = line[start..]
        .split(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '<' || c == '>')
        .next()?
        .trim_end_matches(['.', ',', ')', ';']);
    let u = url::Url::parse(raw).ok()?;
    let fine = u.scheme() == "https"
        && u.host_str() == Some(LINK_HOST)
        && u.port().is_none()
        && u.username().is_empty()
        && u.password().is_none()
        && raw.len() <= 500;
    fine.then(|| u.to_string())
}

/// Has Tailscale share the hub's port as https on the tailnet. Reads the
/// output line by line, so a link to turn on HTTPS comes back at once
/// although Tailscale itself waits for the owner to open it.
async fn serve(program: &Path, port: u16) -> Result<(), Failure> {
    let mut cmd = Command::new(program);
    cmd.args(["serve", "--bg"])
        .arg(port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(not_started)?;
    let out = child.stdout.take().expect("stdout was set to piped");
    let err = child.stderr.take().expect("stderr was set to piped");
    // The child moves in here, so it is killed whenever this is dropped:
    // on the time limit and on an early answer alike.
    let run = async move {
        let mut out = BufReader::new(out).lines();
        let mut err = BufReader::new(err).lines();
        let (mut out_open, mut err_open) = (true, true);
        let mut said = String::new();
        while out_open || err_open {
            let line = tokio::select! {
                l = out.next_line(), if out_open => match l {
                    Ok(Some(l)) => l,
                    _ => { out_open = false; continue; }
                },
                l = err.next_line(), if err_open => match l {
                    Ok(Some(l)) => l,
                    _ => { err_open = false; continue; }
                },
            };
            if let Some(link) = enable_link(&line) {
                tracing::info!("Tailscale needs HTTPS turned on for the tailnet");
                return Err(Failure::HttpsOff(link));
            }
            if said.len() < 2000 {
                said.push_str(&line);
                said.push('\n');
            }
        }
        let done = child.wait().await.map_err(|e| {
            tracing::warn!(error = %e, "lost track of tailscale serve");
            Failure::Failed
        })?;
        if done.success() {
            return Ok(());
        }
        tracing::warn!(output = %short(&said), "tailscale serve failed");
        Err(serve_failure(&said))
    };
    match tokio::time::timeout(Duration::from_secs(SERVE_SECS), run).await {
        Ok(r) => r,
        Err(_) => {
            tracing::warn!("tailscale serve did not finish in time");
            Err(Failure::TooSlow)
        }
    }
}

/// Plain words for a failed `serve`, from what Tailscale said.
fn serve_failure(said: &str) -> Failure {
    let s = said.to_ascii_lowercase();
    if ["logged out", "needslogin", "not logged in", "log in"]
        .iter()
        .any(|w| s.contains(w))
    {
        Failure::SignedOut
    } else if ["access denied", "permission denied", "operator"]
        .iter()
        .any(|w| s.contains(w))
    {
        Failure::NotAllowed
    } else {
        Failure::Failed
    }
}

impl Store {
    /// The address saved by "Connect your phone", if any.
    pub async fn saved_public_url(&self) -> StoreResult<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT value FROM hub_settings WHERE name = 'public_url'")
                .fetch_optional(self.pool())
                .await?,
        )
    }

    /// Saves the address phones and cloud workers use to reach the hub.
    pub async fn save_public_url(&self, url: &str) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO hub_settings (name, value) VALUES ('public_url', ?)
             ON CONFLICT (name) DO UPDATE SET value = excluded.value,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        )
        .bind(url)
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

/// `POST /api/phone/connect`: `{"url": "https://NAME"}`, or a failure as
/// `{"error": "...", "link": "https://..." | null}` with 409 or 503.
pub(crate) async fn connect_route(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("connecting a phone");
    }
    match connect(&st).await {
        Ok(url) => Json(json!({ "url": &*url })).into_response(),
        Err(f) => f.into_response(),
    }
}

/// `GET /api/phone/connection`: the address phones use now, and whether it
/// is https (so notifications work).
pub(crate) async fn connection_route(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<Caller>,
) -> Response {
    if c != Caller::Owner {
        return forbidden("reading the phone connection");
    }
    let url = st.public_url();
    Json(json!({
        "public_url": url.as_deref(),
        "https": url.as_deref().is_some_and(is_https),
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_enable_https_link_is_taken_from_tailscales_words() {
        assert_eq!(
            enable_link("         https://login.tailscale.com/f/serve?node=abc123").as_deref(),
            Some("https://login.tailscale.com/f/serve?node=abc123")
        );
        assert_eq!(
            enable_link("visit https://login.tailscale.com/f/https.").as_deref(),
            Some("https://login.tailscale.com/f/https")
        );
        assert_eq!(enable_link("Serve is not enabled on your tailnet."), None);
        assert_eq!(enable_link("https://evil.example/f/serve"), None);
        assert_eq!(
            enable_link("https://login.tailscale.com.evil.example/x"),
            None
        );
        assert_eq!(enable_link("https://me:pw@login.tailscale.com/x"), None);
        assert_eq!(enable_link("http://login.tailscale.com/x"), None);
    }

    #[test]
    fn the_pcs_name_is_read_only_when_signed_in() {
        let ok = json!({"BackendState": "Running", "Self": {"DNSName": "My-PC.tail1.ts.net."}});
        assert_eq!(read_status(&ok).ok().as_deref(), Some("my-pc.tail1.ts.net"));
        let out = json!({"BackendState": "NeedsLogin", "Self": {"DNSName": ""}});
        assert!(matches!(read_status(&out), Err(Failure::SignedOut)));
        let stopped = json!({"BackendState": "Stopped"});
        assert!(matches!(read_status(&stopped), Err(Failure::SignedOut)));
        let nameless = json!({"BackendState": "Running", "Self": {"DNSName": ""}});
        assert!(matches!(read_status(&nameless), Err(Failure::NoName)));
        let odd = json!({"BackendState": "Running", "Self": {"DNSName": "a b.ts.net."}});
        assert!(matches!(read_status(&odd), Err(Failure::NoName)));
    }
}
