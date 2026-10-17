//! The gatekeeper against a mock Anthropic API on loopback.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method};
use axum::routing::{any, get};
use axum::Router;
use bytes::Bytes;
use decurion_runner::gatekeeper::Gatekeeper;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::sync::mpsc;

const REAL_KEY: &str = "fake-upstream-key-for-tests";

/// What the mock API saw: method, path and query, headers, body.
type Seen = Arc<Mutex<Vec<(Method, String, HeaderMap, Bytes)>>>;

/// A response body fed by hand, one chunk at a time.
struct Chunks(mpsc::Receiver<Bytes>);

impl http_body::Body for Chunks {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        self.0
            .poll_recv(cx)
            .map(|c| c.map(|b| Ok(http_body::Frame::data(b))))
    }
}

async fn record(State((seen, _)): State<(Seen, StreamFeed)>, req: Request) -> &'static str {
    let (parts, body) = req.into_parts();
    let body = axum::body::to_bytes(body, usize::MAX)
        .await
        .unwrap_or_default();
    let pq = parts
        .uri
        .path_and_query()
        .map_or("", |p| p.as_str())
        .to_owned();
    if let Ok(mut s) = seen.lock() {
        s.push((parts.method, pq, parts.headers, body));
    }
    "upstream ok"
}

type StreamFeed = Arc<Mutex<Option<mpsc::Receiver<Bytes>>>>;

async fn stream(State((_, feed)): State<(Seen, StreamFeed)>) -> Body {
    let rx = feed.lock().ok().and_then(|mut f| f.take());
    match rx {
        Some(rx) => Body::new(Chunks(rx)),
        None => Body::empty(),
    }
}

struct Rig {
    gate: Gatekeeper,
    proxy: SocketAddr,
    seen: Seen,
    feed: mpsc::Sender<Bytes>,
    http: reqwest::Client,
}

async fn serve(router: Router) -> SocketAddr {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = l.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(l, router).await });
    addr
}

async fn rig() -> Rig {
    let seen: Seen = Arc::default();
    let (feed, rx) = mpsc::channel(4);
    let upstream = serve(
        Router::new()
            .route("/v1/models", get(stream))
            .fallback(any(record))
            .with_state((seen.clone(), Arc::new(Mutex::new(Some(rx))))),
    )
    .await;
    let gate = Gatekeeper::new(&format!("http://{upstream}")).expect("gatekeeper");
    let proxy = serve(gate.router()).await;
    Rig {
        gate,
        proxy,
        seen,
        feed,
        http: reqwest::Client::new(),
    }
}

impl Rig {
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.proxy)
    }

    fn calls(&self) -> usize {
        self.seen.lock().map(|s| s.len()).unwrap_or(usize::MAX)
    }
}

#[tokio::test]
async fn swaps_the_run_token_for_the_real_key() {
    let r = rig().await;
    let grant = r.gate.grant(REAL_KEY.into());
    let resp = r
        .http
        .post(r.url("/v1/messages?beta=true"))
        .header("x-api-key", grant.token())
        .header("anthropic-version", "2023-06-01")
        .body("{\"hello\":1}")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.expect("body"), "upstream ok");
    let seen = r.seen.lock().expect("seen");
    let (method, pq, headers, body) = &seen[0];
    assert_eq!(method, Method::POST);
    assert_eq!(pq, "/v1/messages?beta=true");
    assert_eq!(&body[..], b"{\"hello\":1}");
    assert_eq!(headers["x-api-key"], REAL_KEY);
    assert_eq!(headers["anthropic-version"], "2023-06-01");
    assert!(!format!("{headers:?}").contains(grant.token()));
}

#[tokio::test]
async fn unknown_tokens_never_reach_the_api() {
    let r = rig().await;
    let _grant = r.gate.grant(REAL_KEY.into());
    for (name, value) in [
        ("x-api-key", "decurion-run-guess".to_owned()),
        ("authorization", "Bearer decurion-run-guess".to_owned()),
        ("x-api-key", REAL_KEY.to_owned()),
    ] {
        let resp = r
            .http
            .post(r.url("/v1/messages"))
            .header(name, value)
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), 401);
    }
    let resp = r.http.post(r.url("/v1/messages")).send().await;
    assert_eq!(resp.expect("send").status(), 401);
    assert_eq!(r.calls(), 0);
}

#[tokio::test]
async fn a_token_dies_with_its_run() {
    let r = rig().await;
    let grant = r.gate.grant(REAL_KEY.into());
    let token = grant.token().to_owned();
    let ok = r
        .http
        .post(r.url("/v1/messages"))
        .header("x-api-key", &token);
    assert_eq!(ok.send().await.expect("send").status(), 200);
    drop(grant);
    let late = r
        .http
        .post(r.url("/v1/messages"))
        .header("x-api-key", &token);
    assert_eq!(late.send().await.expect("send").status(), 401);
    assert_eq!(r.calls(), 1);
}

#[tokio::test]
async fn streams_pass_through_as_they_arrive() {
    let r = rig().await;
    let grant = r.gate.grant(REAL_KEY.into());
    r.feed
        .send(Bytes::from_static(b"event: one\n\n"))
        .await
        .expect("feed");
    let mut resp = r
        .http
        .get(r.url("/v1/models"))
        .header("x-api-key", grant.token())
        .send()
        .await
        .expect("send");
    // The second chunk is only written after the first one has arrived, so
    // a proxy that waited for the whole body would hang here.
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), resp.chunk())
        .await
        .expect("first chunk was held back")
        .expect("chunk");
    assert_eq!(first.as_deref(), Some(&b"event: one\n\n"[..]));
    r.feed
        .send(Bytes::from_static(b"event: two\n\n"))
        .await
        .expect("feed");
    let second = resp.chunk().await.expect("chunk");
    assert_eq!(second.as_deref(), Some(&b"event: two\n\n"[..]));
}

#[tokio::test]
async fn only_api_paths_are_forwarded() {
    let r = rig().await;
    let grant = r.gate.grant(REAL_KEY.into());
    for path in [
        "/",
        "/api/hello",
        "/v1",
        "/v1/../api/hello",
        "/v1/%2e%2e/api/hello",
        "//evil.example/v1/messages",
    ] {
        let resp = r
            .http
            .get(r.url(path))
            .header("x-api-key", grant.token())
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), 404, "{path}");
    }
    assert_eq!(r.calls(), 0);
}

#[tokio::test]
async fn client_credentials_are_not_passed_on() {
    let r = rig().await;
    let grant = r.gate.grant(REAL_KEY.into());
    let bearer = format!("Bearer {}", grant.token());
    let resp = r
        .http
        .post(r.url("/v1/messages"))
        .header("authorization", &bearer)
        .header("proxy-authorization", "Basic c2VjcmV0")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 200);
    let seen = r.seen.lock().expect("seen");
    let headers = &seen[0].2;
    assert_eq!(headers.get_all("x-api-key").iter().count(), 1);
    assert_eq!(headers["x-api-key"], REAL_KEY);
    assert!(headers.get("authorization").is_none());
    assert!(headers.get("proxy-authorization").is_none());
}

#[tokio::test]
async fn only_the_calls_claude_code_makes_are_forwarded() {
    let r = rig().await;
    let grant = r.gate.grant(REAL_KEY.into());
    // Batches keep billing after the run; the Files API reaches other runs.
    for (method, path) in [
        ("POST", "/v1/messages/batches"),
        ("GET", "/v1/messages/batches"),
        ("GET", "/v1/files"),
        ("POST", "/v1/files"),
        ("DELETE", "/v1/files/file_123"),
        ("GET", "/v1/messages"),
        ("DELETE", "/v1/messages"),
        ("POST", "/v1/models"),
        ("GET", "/v1/models/a%2Fb"),
        ("GET", "/v1/models/../files"),
        ("POST", "/v1/complete"),
    ] {
        let resp = r
            .http
            .request(method.parse().expect("method"), r.url(path))
            .header("x-api-key", grant.token())
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), 404, "{method} {path}");
    }
    assert_eq!(r.calls(), 0);
    for (method, path) in [
        ("POST", "/v1/messages"),
        ("POST", "/v1/messages/count_tokens"),
        ("GET", "/v1/models/claude-sonnet-x"),
    ] {
        let resp = r
            .http
            .request(method.parse().expect("method"), r.url(path))
            .header("x-api-key", grant.token())
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), 200, "{method} {path}");
    }
    assert_eq!(r.calls(), 3);
}

#[tokio::test]
async fn oversized_requests_are_refused() {
    let r = rig().await;
    let grant = r.gate.grant(REAL_KEY.into());
    let resp = r
        .http
        .post(r.url("/v1/messages"))
        .header("x-api-key", grant.token())
        .body(vec![b'x'; (32 << 20) + 1])
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 413);
    assert_eq!(r.calls(), 0);
}

/// Paths sent to the gatekeeper exactly as written, without a client
/// normalising them first.
#[tokio::test]
async fn raw_traversal_paths_are_refused() {
    use tower::ServiceExt;
    let r = rig().await;
    let grant = r.gate.grant(REAL_KEY.into());
    for path in [
        "/v1/../api/hello",
        "/v1/messages/../files",
        "/v1/%2e%2e/api/hello",
        "/v1/messages%2Fbatches",
        "/v1\\messages",
    ] {
        let req = axum::http::Request::builder()
            .method("POST")
            .uri(path)
            .header("x-api-key", grant.token())
            .body(axum::body::Body::empty())
            .expect("request");
        let resp = r.gate.router().oneshot(req).await.expect("response");
        assert_eq!(resp.status(), 404, "{path}");
    }
    assert_eq!(r.calls(), 0);
}

#[tokio::test]
async fn a_streaming_answer_stops_when_its_run_ends() {
    let r = rig().await;
    let grant = r.gate.grant(REAL_KEY.into());
    r.feed
        .send(Bytes::from_static(b"event: one\n\n"))
        .await
        .expect("feed");
    let mut resp = r
        .http
        .get(r.url("/v1/models"))
        .header("x-api-key", grant.token())
        .send()
        .await
        .expect("send");
    let first = resp.chunk().await.expect("chunk");
    assert_eq!(first.as_deref(), Some(&b"event: one\n\n"[..]));
    drop(grant);
    r.feed
        .send(Bytes::from_static(b"event: two\n\n"))
        .await
        .expect("feed");
    let rest = tokio::time::timeout(std::time::Duration::from_secs(5), resp.chunk())
        .await
        .expect("stream did not end");
    assert!(!matches!(rest, Ok(Some(ref b)) if b.as_ref() == b"event: two\n\n"));
}

/// A mock Messages API whose answer depends on the `x-mock` header.
async fn priced_api() -> (Gatekeeper, SocketAddr, Arc<Mutex<u32>>) {
    let calls: Arc<Mutex<u32>> = Arc::default();
    let counted = calls.clone();
    let upstream = serve(Router::new().route(
        "/v1/messages",
        axum::routing::post(move |h: HeaderMap| {
            let counted = counted.clone();
            async move {
                if let Ok(mut n) = counted.lock() {
                    *n += 1;
                }
                let mode = h.get("x-mock").and_then(|v| v.to_str().ok()).unwrap_or("");
                let sse = |body: &'static str| {
                    axum::http::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(Body::from(body))
                        .expect("response")
                };
                match mode {
                    // Opus 5.5: 1000 in, 10000 cache read, 2000 5m writes, 400 out.
                    "sse" => sse(concat!(
                        "event: message_start\n",
                        "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-opus-5-5\",",
                        "\"usage\":{\"input_tokens\":1000,\"cache_read_input_tokens\":10000,",
                        "\"cache_creation\":{\"ephemeral_5m_input_tokens\":2000,\"ephemeral_1h_input_tokens\":0},",
                        "\"output_tokens\":1}}}\n\n",
                        "event: content_block_delta\n",
                        "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
                        "event: message_delta\n",
                        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":400}}\n\n",
                    )),
                    // A model the table does not know: priced at the top rates.
                    "unknown" => sse(concat!(
                        "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-new-9\",",
                        "\"usage\":{\"input_tokens\":1000,\"output_tokens\":0}}}\n\n",
                    )),
                    "error" => axum::http::Response::builder()
                        .status(529)
                        .header("content-type", "application/json")
                        .body(Body::from("{\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}"))
                        .expect("response"),
                    "garbled" => axum::http::Response::builder()
                        .header("content-type", "application/json")
                        .body(Body::from("not json"))
                        .expect("response"),
                    // Sonnet 5.5, not streamed: 2000 in, 1000 out.
                    _ => axum::http::Response::builder()
                        .header("content-type", "application/json")
                        .body(Body::from(
                            "{\"model\":\"claude-sonnet-5-5\",\"usage\":{\"input_tokens\":2000,\"output_tokens\":1000}}",
                        ))
                        .expect("response"),
                }
            }
        }),
    ))
    .await;
    let gate = Gatekeeper::new(&format!("http://{upstream}")).expect("gatekeeper");
    let proxy = serve(gate.router()).await;
    (gate, proxy, calls)
}

async fn ask(proxy: SocketAddr, token: &str, mode: &str) -> reqwest::StatusCode {
    let resp = reqwest::Client::new()
        .post(format!("http://{proxy}/v1/messages"))
        .header("x-api-key", token)
        .header("x-mock", mode)
        .body("{}")
        .send()
        .await
        .expect("send");
    let status = resp.status();
    // Read to the end, as Claude Code does; the meter settles when it ends.
    let _ = resp.bytes().await;
    status
}

/// The meter adds its total when the answer finishes streaming; wait (up to
/// two seconds) for it to land.
async fn settled(grant: &decurion_runner::gatekeeper::Grant) -> u64 {
    let mut last = grant.spent_micros();
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let now = grant.spent_micros();
        if now == last && now > 0 {
            return now;
        }
        last = now;
    }
    last
}

#[tokio::test]
async fn streamed_answers_are_priced_from_their_usage() {
    let (gate, proxy, _) = priced_api().await;
    let grant = gate.grant_with_budget(REAL_KEY.into(), Some(10_000_000));
    assert_eq!(ask(proxy, grant.token(), "sse").await, 200);
    // 1000*4 + 10000*0.2 + 2000*4*1.25 + 400*20 micro-dollars.
    assert_eq!(settled(&grant).await, 4_000 + 2_000 + 10_000 + 8_000);
}

#[tokio::test]
async fn plain_answers_are_priced_too() {
    let (gate, proxy, _) = priced_api().await;
    let grant = gate.grant_with_budget(REAL_KEY.into(), Some(10_000_000));
    assert_eq!(ask(proxy, grant.token(), "json").await, 200);
    // Sonnet 5.5: 2000*2 + 1000*10.
    assert_eq!(settled(&grant).await, 4_000 + 10_000);
}

#[tokio::test]
async fn unknown_models_are_priced_at_the_top_rate() {
    let (gate, proxy, _) = priced_api().await;
    let grant = gate.grant_with_budget(REAL_KEY.into(), Some(10_000_000));
    ask(proxy, grant.token(), "unknown").await;
    assert_eq!(settled(&grant).await, 15_000);
}

#[tokio::test]
async fn errors_cost_nothing_and_unreadable_answers_cost_everything_left() {
    let (gate, proxy, _) = priced_api().await;
    let grant = gate.grant_with_budget(REAL_KEY.into(), Some(1_000_000));
    assert_eq!(ask(proxy, grant.token(), "error").await, 529);
    assert_eq!(settled(&grant).await, 0);
    ask(proxy, grant.token(), "garbled").await;
    assert_eq!(settled(&grant).await, 1_000_000);
    assert!(grant.used_up());
}

#[tokio::test]
async fn calls_stop_once_the_run_has_spent_its_money() {
    let (gate, proxy, calls) = priced_api().await;
    // Room for one streamed answer (24,000) but not a second.
    let grant = gate.grant_with_budget(REAL_KEY.into(), Some(30_000));
    assert_eq!(ask(proxy, grant.token(), "sse").await, 200);
    settled(&grant).await;
    assert_eq!(ask(proxy, grant.token(), "sse").await, 200);
    settled(&grant).await;
    assert!(grant.used_up());
    // Refused at the gatekeeper: the API is not called again.
    let before = calls.lock().map(|n| *n).unwrap_or(0);
    assert_eq!(ask(proxy, grant.token(), "sse").await, 400);
    assert_eq!(calls.lock().map(|n| *n).unwrap_or(0), before);
    // Counting tokens and listing models cost nothing and stay allowed.
    let r = reqwest::Client::new()
        .get(format!("http://{proxy}/v1/models"))
        .header("x-api-key", grant.token())
        .send()
        .await
        .expect("send");
    assert_ne!(r.status(), 400);
}
