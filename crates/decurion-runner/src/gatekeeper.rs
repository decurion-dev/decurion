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
//! token.
//!
//! - Before a call is sent, its worst case (input at twice the input price,
//!   plus `max_tokens` of output) is held from the run's money; a call whose
//!   worst case does not fit is refused. When the answer ends, the hold is
//!   swapped for the real cost.
//! - Answers are asked for uncompressed so they can be read. One that ends
//!   before its final usage count (the client hung up, or the run ended) is
//!   charged its whole `max_tokens`; one that cannot be read at all is charged
//!   the run's whole budget.
//! - Request settings the meter cannot price (fallbacks, the advisor tool,
//!   fast mode, priority tier, regional inference, code execution, MCP) are
//!   refused, so a new way of being billed fails closed.

use crate::pricing::{cost_micros, read_usage, Usage};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Response, StatusCode};
use axum::Router;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const ANTHROPIC_API: &str = "https://api.anthropic.com";

/// Requests in flight at once; more wait their turn.
const MAX_IN_FLIGHT: usize = 16;
/// Requests in flight at once for one run.
pub const PER_RUN_IN_FLIGHT: usize = 4;
/// Longest a single call may take, streaming included.
const CALL_DEADLINE: Duration = Duration::from_secs(1800);
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
    // Answers must come back uncompressed so the meter can read them.
    "accept-encoding",
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
    money: Mutex<Money>,
    /// Set when a call was refused because it did not fit the budget.
    stopped: AtomicBool,
    in_flight: Arc<Semaphore>,
}

/// A run's money, in micro-dollars.
#[derive(Default)]
struct Money {
    /// What finished answers cost.
    spent: u64,
    /// Worst cases held for calls still in flight.
    held: u64,
}

impl Run {
    fn money(&self) -> MutexGuard<'_, Money> {
        self.money.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Holds `worst` for a call, or refuses it if that would not fit.
    fn hold(&self, worst: u64) -> bool {
        let mut m = self.money();
        if let Some(b) = self.budget {
            if m.spent.saturating_add(m.held).saturating_add(worst) > b {
                self.stopped.store(true, Ordering::SeqCst);
                return false;
            }
        }
        m.held = m.held.saturating_add(worst);
        true
    }

    /// Swaps a call's hold for what it really cost.
    fn settle_call(&self, worst: u64, cost: u64) {
        let mut m = self.money();
        m.held = m.held.saturating_sub(worst);
        m.spent = m.spent.saturating_add(cost);
    }

    /// Charges the whole budget: an answer was paid for at an unknown size.
    fn charge_everything(&self, worst: u64) {
        let mut m = self.money();
        m.held = m.held.saturating_sub(worst);
        if let Some(b) = self.budget {
            m.spent = m.spent.max(b);
        }
        self.stopped.store(true, Ordering::SeqCst);
    }
}

impl Grant {
    /// The token the container presents as its API key.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// What the run's answers have cost so far, in micro-dollars.
    pub fn spent_micros(&self) -> u64 {
        self.run.money().spent
    }

    /// True once a call was refused for not fitting the run's money.
    pub fn used_up(&self) -> bool {
        self.run.stopped.load(Ordering::SeqCst)
    }

    /// Ends the run's access, then waits up to `limit` for calls still in
    /// flight to wind down, so every answer's cost has been added. False if
    /// they did not finish in time.
    pub async fn finish(&self, limit: Duration) -> bool {
        self.revoke();
        let all = PER_RUN_IN_FLIGHT as u32;
        matches!(
            tokio::time::timeout(limit, self.run.in_flight.clone().acquire_many_owned(all)).await,
            Ok(Ok(_))
        )
    }

    fn revoke(&self) {
        // Requests already in flight see this and stop too.
        self.run.live.store(false, Ordering::SeqCst);
        self.gate.runs().remove(&self.token);
    }
}

impl Drop for Grant {
    fn drop(&mut self) {
        self.revoke();
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
            .timeout(CALL_DEADLINE)
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
            money: Mutex::new(Money::default()),
            stopped: AtomicBool::new(false),
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
    // This run's calls wait their turn first, so one busy run cannot hold the
    // shared slots; both are held until the answer has finished streaming.
    let Ok(run_permit) = run.in_flight.clone().acquire_owned().await else {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "overloaded_error", "busy");
    };
    let Ok(permit) = g.inner.in_flight.clone().acquire_owned().await else {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "overloaded_error", "busy");
    };
    let metered = url.path() == "/v1/messages";
    let mut worst = 0;
    let mut max_tokens = MAX_OUTPUT;
    if metered {
        let Ok(req_json) = serde_json::from_slice::<serde_json::Value>(&body) else {
            return refuse(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "request body is not JSON",
            );
        };
        if let Some(why) = unpriced(&req_json) {
            return refuse(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &format!("Decurion: {why} is not allowed on pay-per-use runs"),
            );
        }
        max_tokens = req_json["max_tokens"].as_u64().unwrap_or(MAX_OUTPUT);
        worst = worst_case_micros(&req_json, body.len(), max_tokens);
        if !run.hold(worst) {
            // Not a retryable error: Claude Code stops, and the runner sends
            // the work to the owner.
            return refuse(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "Decurion: this run's spending limit is used up",
            );
        }
    }
    // From here every exit gives the hold back.
    let mut hold = Hold {
        run: run.clone(),
        worst,
    };
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
    headers.insert(
        header::ACCEPT_ENCODING,
        HeaderValue::from_static("identity"),
    );
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
            let connected = !err.is_connect();
            tracing::warn!(err = %err.without_url(), "gatekeeper could not reach the API");
            // If the request went out, Anthropic may have done (and billed)
            // work before the failure: keep the hold as the charge. Only a
            // failure to connect at all is free.
            if connected {
                hold.charge_worst();
            }
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
    // however it ends. The meter holds the permits, so the run's slots free
    // up only after its cost is counted.
    let permits = (run_permit, permit);
    let mut meter = if metered && status.is_success() {
        Some(Meter::new(hold.take(), sse, max_tokens, permits))
    } else {
        // Errors are not billed; the hold is given back.
        drop(permits);
        None
    };
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

/// A call's worst-case hold on its run's money, given back if the call ends
/// without an answer to meter.
struct Hold {
    run: Arc<Run>,
    worst: u64,
}

impl Hold {
    /// Charges the call's whole worst case: its outcome is unknown.
    fn charge_worst(&mut self) {
        let worst = std::mem::take(&mut self.worst);
        self.run.settle_call(worst, worst);
    }

    /// Hands the hold to the meter, which swaps it for the real cost.
    fn take(&mut self) -> (Arc<Run>, u64) {
        (self.run.clone(), std::mem::take(&mut self.worst))
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        if self.worst > 0 {
            self.run.settle_call(self.worst, 0);
        }
    }
}

/// A request setting that bills in a way the meter cannot price.
fn unpriced(req: &serde_json::Value) -> Option<&'static str> {
    let set = |k: &str| !req[k].is_null();
    if set("fallbacks") {
        return Some("fallbacks");
    }
    if set("speed") && req["speed"] != "standard" {
        return Some("fast mode");
    }
    if set("service_tier") && req["service_tier"] != "standard_only" {
        return Some("a service tier");
    }
    if set("inference_geo") && req["inference_geo"] != "global" {
        return Some("regional inference");
    }
    if set("mcp_servers") || set("container") {
        return Some("MCP servers or containers");
    }
    let tools = req["tools"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    for t in tools {
        let kind = t["type"].as_str().unwrap_or("");
        if ["advisor", "code_execution", "mcp_toolset"]
            .iter()
            .any(|p| kind.starts_with(p))
        {
            return Some("that tool");
        }
    }
    None
}

/// Searches assumed for a web search tool that does not set `max_uses`.
const SEARCHES_ASSUMED: u64 = 20;

/// The most a call could cost, for holding money before it is sent: every
/// request byte as an input token at twice the input price (the dearest cache
/// write), plus `max_tokens` of output, plus web searches. Documents such as
/// PDFs can expand past their byte size; the real cost is still recorded in
/// full when the answer arrives, so such a call is counted, just not held
/// for exactly.
fn worst_case_micros(req: &serde_json::Value, body_len: usize, max_tokens: u64) -> u64 {
    let p = crate::pricing::worst_price(req["model"].as_str().unwrap_or(""));
    let searches: u64 = req["tools"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|t| {
            t["type"]
                .as_str()
                .is_some_and(|k| k.starts_with("web_search"))
        })
        .map(|t| t["max_uses"].as_u64().unwrap_or(SEARCHES_ASSUMED))
        .sum();
    (body_len as f64 * p.input * 2.0 + max_tokens as f64 * p.output).ceil() as u64
        + searches * crate::pricing::WEB_SEARCH_MICROS
}

/// Reads the usage out of one answer as it passes through and, when the
/// answer is done (or dropped part-way), swaps the call's hold for its cost.
struct Meter {
    run: Arc<Run>,
    /// The call's worst-case hold.
    worst: u64,
    sse: bool,
    /// A plain answer, kept whole to read at the end.
    body: Vec<u8>,
    /// The answer, or one of its lines, was too big to read.
    too_big: bool,
    /// The stream line being read.
    line: Vec<u8>,
    model: String,
    usage: Usage,
    /// The request's `max_tokens`: what an unfinished answer is charged.
    max_tokens: u64,
    /// Whether the stream's opening usage arrived.
    started: bool,
    /// Whether the stream reached its final usage count.
    finished: bool,
    /// Released only after the cost is added (fields drop after `drop`).
    _permits: (OwnedSemaphorePermit, OwnedSemaphorePermit),
}

/// Largest `max_tokens` any model accepts, assumed when a request omits it.
const MAX_OUTPUT: u64 = 128_000;

impl Meter {
    fn new(
        (run, worst): (Arc<Run>, u64),
        sse: bool,
        max_tokens: u64,
        permits: (OwnedSemaphorePermit, OwnedSemaphorePermit),
    ) -> Self {
        Self {
            run,
            worst,
            sse,
            body: Vec::new(),
            too_big: false,
            line: Vec::new(),
            model: String::new(),
            usage: Usage::default(),
            max_tokens,
            started: false,
            finished: false,
            _permits: permits,
        }
    }

    fn feed(&mut self, mut bytes: &[u8]) {
        if !self.sse {
            if self.body.len() + bytes.len() <= MAX_JSON {
                self.body.extend_from_slice(bytes);
            } else {
                self.too_big = true;
            }
            return;
        }
        while let Some(i) = bytes.iter().position(|b| *b == b'\n') {
            self.keep(&bytes[..i]);
            self.read_line();
            self.line.clear();
            bytes = &bytes[i + 1..];
        }
        self.keep(bytes);
    }

    fn keep(&mut self, part: &[u8]) {
        let room = MAX_LINE.saturating_sub(self.line.len());
        if part.len() > room {
            self.too_big = true;
        }
        self.line.extend_from_slice(&part[..part.len().min(room)]);
    }

    /// Streams report usage in `message_start` (input, cache, first output)
    /// and `message_delta` (running output total, and the stop reason).
    /// Content lines are skipped without parsing.
    fn read_line(&mut self) {
        let Some(data) = self.line.strip_prefix(b"data:") else {
            return;
        };
        let has = |needle: &[u8]| data.windows(needle.len()).any(|w| w == needle);
        if !(has(b"message_start") || has(b"message_delta") || has(b"message_stop")) {
            return;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) else {
            return;
        };
        match v["type"].as_str() {
            Some("message_start") => {
                if let Some(m) = v["message"]["model"].as_str() {
                    self.model = m.to_owned();
                }
                read_usage(&v["message"]["usage"], &mut self.usage);
                self.started = true;
            }
            Some("message_delta") => {
                read_usage(&v["usage"], &mut self.usage);
                if !v["delta"]["stop_reason"].is_null() {
                    self.finished = true;
                }
            }
            Some("message_stop") => self.finished = true,
            _ => {}
        }
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        if self.sse {
            self.read_line();
            if !self.started || self.too_big {
                // Nothing readable came through: charged as unknown.
                self.run.charge_everything(self.worst);
                return;
            }
            if !self.finished {
                // Cut off before the final count: later input (server tools)
                // and output are unknown, so the call is charged at least its
                // whole worst case.
                self.usage.output = self.usage.output.max(self.max_tokens);
                let cost = cost_micros(&self.model, &self.usage).max(self.worst);
                self.run.settle_call(self.worst, cost);
                return;
            }
        } else {
            match serde_json::from_slice::<serde_json::Value>(&self.body) {
                Ok(v) if !self.too_big && v["usage"].is_object() => {
                    if let Some(m) = v["model"].as_str() {
                        self.model = m.to_owned();
                    }
                    read_usage(&v["usage"], &mut self.usage);
                }
                _ => {
                    self.run.charge_everything(self.worst);
                    return;
                }
            }
        }
        self.run
            .settle_call(self.worst, cost_micros(&self.model, &self.usage));
    }
}
