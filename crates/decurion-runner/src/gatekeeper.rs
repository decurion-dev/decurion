//! The gatekeeper: a small proxy on the host that holds the pay-per-use API
//! key, so the key itself never enters a container.
//!
//! Each pay-per-use run gets a random token instead of the key. The proxy
//! swaps a live token for the real key and forwards the request to Anthropic;
//! the token dies with the run, so one the agent leaks is worth nothing.
//!
//! Only the calls Claude Code makes are forwarded (send a message, count
//! tokens, list models). Batches would keep billing after the run, and the
//! Files API would reach other runs' files, so they are refused.
//!
//! Every `/v1/messages` answer is priced from the usage it reports (see
//! [`crate::pricing`]) and counted against the run's set-aside money, whoever
//! made the call: Claude Code, or an agent calling the API directly with its
//! token. Once that money is used, further calls are refused. Calls already in
//! flight when it runs out still finish, so a run can go over by what those
//! cost; at most [`PER_RUN_IN_FLIGHT`] are allowed at once to keep that small.

use crate::pricing::{cost_micros, read_usage, Usage};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Response, StatusCode};
use axum::Router;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::Semaphore;

pub const ANTHROPIC_API: &str = "https://api.anthropic.com";

/// Requests in flight at once; more wait their turn.
const MAX_IN_FLIGHT: usize = 16;
/// Requests in flight at once for one run, which bounds how far past its
/// money a run can go.
pub const PER_RUN_IN_FLIGHT: usize = 4;
/// Most of a plain (non-streamed) answer kept to read its usage.
const MAX_JSON: usize = 16 << 20;
/// Longest stream line kept to read its usage.
const MAX_LINE: usize = 1 << 20;
/// Largest request body passed on. Claude Code's requests are far smaller.
const MAX_BODY: usize = 32 << 20;

/// Whether `method path` is a call Claude Code makes.
fn allowed(method: &axum::http::Method, path: &str) -> bool {
    use axum::http::Method;
    match (method, path) {
        (&Method::POST, "/v1/messages" | "/v1/messages/count_tokens") => true,
        (&Method::GET, "/v1/models") => true,
        (&Method::GET, p) => p.strip_prefix("/v1/models/").is_some_and(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
        }),
        _ => false,
    }
}

/// Headers that describe one connection, not the request, plus every way a
/// client can present a credential. None of them are passed on.
const DROPPED: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
    "authorization",
    "x-api-key",
];

#[derive(Clone)]
pub struct Gatekeeper {
    inner: Arc<Inner>,
}

struct Inner {
    upstream: reqwest::Url,
    http: reqwest::Client,
    /// Live run tokens, each with the real key it stands for and the run's
    /// state.
    runs: Mutex<HashMap<String, (String, Arc<Run>)>>,
    in_flight: Arc<Semaphore>,
    /// False once the server has stopped; the runner checks it before
    /// starting pay-per-use work.
    alive: AtomicBool,
}

/// One run's permission to use the key through the gatekeeper. Dropping it
/// revokes the token, so every way out of a run ends it.
pub struct Grant {
    token: String,
    run: Arc<Run>,
    gate: Gatekeeper,
}

/// One run's state, shared by its in-flight requests.
struct Run {
    /// False the moment the run ends.
    live: AtomicBool,
    /// What the run may spend, in micro-dollars; `None` means no limit.
    budget: Option<u64>,
    /// What its answers have cost so far, in micro-dollars.
    spent: AtomicU64,
    in_flight: Arc<Semaphore>,
}

impl Run {
    fn used_up(&self) -> bool {
        self.budget
            .is_some_and(|b| self.spent.load(Ordering::SeqCst) >= b)
    }
}

impl Grant {
    /// The token the container presents as its API key.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// What the run's answers have cost so far, in micro-dollars.
    pub fn spent_micros(&self) -> u64 {
        self.run.spent.load(Ordering::SeqCst)
    }

    /// True once the run has spent all it was allowed.
    pub fn used_up(&self) -> bool {
        self.run.used_up()
    }
}

impl Drop for Grant {
    fn drop(&mut self) {
        // Requests already in flight see this and stop too.
        self.run.live.store(false, Ordering::SeqCst);
        self.gate.runs().remove(&self.token);
    }
}

impl Gatekeeper {
    /// A gatekeeper that forwards to `upstream` (an origin such as
    /// [`ANTHROPIC_API`], with no path).
    pub fn new(upstream: &str) -> anyhow::Result<Self> {
        let upstream = reqwest::Url::parse(upstream)?;
        if upstream.path() != "/" || upstream.query().is_some() {
            anyhow::bail!("upstream {upstream} must be an origin with no path");
        }
        let http = reqwest::Client::builder()
            // A redirect would carry the key to wherever it points.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(30))
            // Long enough for a streamed answer; a stalled one is dropped.
            .read_timeout(Duration::from_secs(600))
            .build()?;
        Ok(Self {
            inner: Arc::new(Inner {
                upstream,
                http,
                runs: Mutex::new(HashMap::new()),
                in_flight: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
                alive: AtomicBool::new(true),
            }),
        })
    }

    /// Mints a fresh token that stands for `real_key` until the grant drops,
    /// with no spending limit.
    pub fn grant(&self, real_key: String) -> Grant {
        self.grant_with_budget(real_key, None)
    }

    /// Like [`Gatekeeper::grant`], but calls are refused once the run's
    /// answers have cost `budget_micros`.
    pub fn grant_with_budget(&self, real_key: String, budget_micros: Option<u64>) -> Grant {
        let token = format!(
            "decurion-run-{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let run = Arc::new(Run {
            live: AtomicBool::new(true),
            budget: budget_micros,
            spent: AtomicU64::new(0),
            in_flight: Arc::new(Semaphore::new(PER_RUN_IN_FLIGHT)),
        });
        self.runs().insert(token.clone(), (real_key, run.clone()));
        Grant {
            token,
            run,
            gate: self.clone(),
        }
    }

    pub fn router(&self) -> Router {
        Router::new().fallback(forward).with_state(self.clone())
    }

    /// Serves until the listener fails; after that [`Gatekeeper::is_alive`]
    /// is false.
    pub async fn serve(self, listener: tokio::net::TcpListener) -> std::io::Result<()> {
        let r = axum::serve(listener, self.router()).await;
        self.inner.alive.store(false, Ordering::SeqCst);
        r
    }

    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::SeqCst)
    }

    fn runs(&self) -> MutexGuard<'_, HashMap<String, (String, Arc<Run>)>> {
        // The map stays consistent even if a holder panicked.
        self.inner.runs.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn key_for(&self, token: &str) -> Option<(String, Arc<Run>)> {
        self.runs().get(token).cloned()
    }
}

/// The token a client presented, as `x-api-key` or a bearer token.
fn presented(h: &HeaderMap) -> Option<&str> {
    if let Some(k) = h.get("x-api-key").and_then(|v| v.to_str().ok()) {
        return Some(k);
    }
    h.get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

/// Headers to pass on in either direction: everything but the dropped ones
/// and any the `Connection` header names.
fn passable(h: &HeaderMap) -> HeaderMap {
    let named: Vec<String> = h
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|s| s.trim().to_ascii_lowercase())
        .collect();
    let mut out = HeaderMap::new();
    for (k, v) in h {
        let name = k.as_str();
        if !DROPPED.contains(&name) && !named.iter().any(|n| n == name) {
            out.append(k.clone(), v.clone());
        }
    }
    out
}

/// An error in the shape Anthropic's API uses, so Claude Code reports it
/// sensibly.
fn refuse(status: StatusCode, kind: &str, message: &str) -> Response<Body> {
    let body = serde_json::json!({
        "type": "error",
        "error": { "type": kind, "message": message },
    });
    let mut r = Response::new(Body::from(body.to_string()));
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    r
}

async fn forward(State(g): State<Gatekeeper>, req: Request) -> Response<Body> {
    let (parts, body) = req.into_parts();
    let pq = parts.uri.path_and_query().map_or("/", |p| p.as_str());
    // Encoded or backslashed paths are never ours; refuse them rather than
    // hope the far end reads them the same way.
    let raw_path = parts.uri.path();
    // Parse against the upstream so `..` and friends are resolved before the
    // path is checked.
    let url = match g.inner.upstream.join(pq) {
        Ok(u)
            if !raw_path.contains(['%', '\\'])
                && u.origin() == g.inner.upstream.origin()
                && allowed(&parts.method, u.path()) =>
        {
            u
        }
        _ => return refuse(StatusCode::NOT_FOUND, "not_found_error", "not forwarded"),
    };
    let Some((key, run)) = presented(&parts.headers).and_then(|t| g.key_for(t)) else {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "unknown or expired run token",
        );
    };
    let Ok(mut key) = HeaderValue::from_str(&key) else {
        return refuse(
            StatusCode::INTERNAL_SERVER_ERROR,
            "api_error",
            "the saved API key is not a valid header value",
        );
    };
    key.set_sensitive(true);
    let Ok(body) = axum::body::to_bytes(body, MAX_BODY).await else {
        return refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_too_large",
            "request too large",
        );
    };
    // Held until the answer has finished streaming.
    let Ok(permit) = g.inner.in_flight.clone().acquire_owned().await else {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "overloaded_error", "busy");
    };
    let metered = url.path() == "/v1/messages";
    // Each run's calls wait their turn, then the global limit.
    let Ok(run_permit) = run.in_flight.clone().acquire_owned().await else {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "overloaded_error", "busy");
    };
    if metered && run.used_up() {
        // Not a retryable error: Claude Code stops, and the runner sends the
        // work to the owner.
        return refuse(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "Decurion: this run's spending limit is used up",
        );
    }
    // The body may have taken a while: the run must still be going.
    if !run.live.load(Ordering::SeqCst) {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "unknown or expired run token",
        );
    }
    let mut headers = passable(&parts.headers);
    headers.insert(HeaderName::from_static("x-api-key"), key);
    let sent = g
        .inner
        .http
        .request(parts.method, url)
        .headers(headers)
        .body(body)
        .send()
        .await;
    let up = match sent {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(err = %err.without_url(), "gatekeeper could not reach the API");
            return refuse(
                StatusCode::BAD_GATEWAY,
                "api_error",
                "could not reach the API",
            );
        }
    };
    let status = up.status();
    let headers = passable(up.headers());
    let sse = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"));
    // Priced as it streams past; the total is added when the answer ends,
    // however it ends.
    let mut meter = (metered && status.is_success()).then(|| Meter::new(run.clone(), sse));
    let live = run.clone();
    // Passed through as it arrives: Claude Code reads server-sent events.
    // Cut off an answer still streaming when its run ends.
    let stream = up
        .bytes_stream()
        .take_while(move |_| {
            let go = live.live.load(Ordering::SeqCst);
            async move { go }
        })
        .map(move |chunk| {
            let _held = (&permit, &run_permit);
            if let (Some(m), Ok(bytes)) = (meter.as_mut(), &chunk) {
                m.feed(bytes);
            }
            chunk
        });
    let mut r = Response::new(Body::from_stream(stream));
    *r.status_mut() = status;
    *r.headers_mut() = headers;
    r
}

/// Reads the usage out of one answer as it passes through and, when the
/// answer is done (or dropped part-way), adds its cost to the run.
struct Meter {
    run: Arc<Run>,
    sse: bool,
    /// A plain answer, kept whole to read at the end.
    body: Vec<u8>,
    too_big: bool,
    /// The stream line being read.
    line: Vec<u8>,
    model: String,
    usage: Usage,
}

impl Meter {
    fn new(run: Arc<Run>, sse: bool) -> Self {
        Self {
            run,
            sse,
            body: Vec::new(),
            too_big: false,
            line: Vec::new(),
            model: String::new(),
            usage: Usage::default(),
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        if !self.sse {
            if self.body.len() + bytes.len() <= MAX_JSON {
                self.body.extend_from_slice(bytes);
            } else {
                self.too_big = true;
            }
            return;
        }
        for &b in bytes {
            if b == b'\n' {
                self.read_line();
                self.line.clear();
            } else if self.line.len() < MAX_LINE {
                self.line.push(b);
            }
        }
    }

    /// Streams report usage in `message_start` (input, cache, first output)
    /// and `message_delta` (running output total).
    fn read_line(&mut self) {
        let Some(data) = self.line.strip_prefix(b"data:") else {
            return;
        };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) else {
            return;
        };
        match v["type"].as_str() {
            Some("message_start") => {
                if let Some(m) = v["message"]["model"].as_str() {
                    self.model = m.to_owned();
                }
                read_usage(&v["message"]["usage"], &mut self.usage);
            }
            Some("message_delta") => read_usage(&v["usage"], &mut self.usage),
            _ => {}
        }
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        if self.sse {
            self.read_line();
        } else {
            match serde_json::from_slice::<serde_json::Value>(&self.body) {
                Ok(v) if !self.too_big => {
                    if let Some(m) = v["model"].as_str() {
                        self.model = m.to_owned();
                    }
                    read_usage(&v["usage"], &mut self.usage);
                }
                // An answer that cannot be read is charged the rest of the
                // run's money: it was paid for, and its size is unknown.
                _ => {
                    if let Some(b) = self.run.budget {
                        let spent = self.run.spent.load(Ordering::SeqCst);
                        self.run
                            .spent
                            .fetch_add(b.saturating_sub(spent), Ordering::SeqCst);
                    }
                    return;
                }
            }
        }
        self.run
            .spent
            .fetch_add(cost_micros(&self.model, &self.usage), Ordering::SeqCst);
    }
}
