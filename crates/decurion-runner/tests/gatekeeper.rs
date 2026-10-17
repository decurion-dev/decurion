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

const REAL_KEY: &str = "sk-ant-test-real-key";

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
            .route("/v1/stream", get(stream))
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
        .get(r.url("/v1/stream"))
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
