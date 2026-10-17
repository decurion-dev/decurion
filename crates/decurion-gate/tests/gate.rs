//! Runs a real gate on 127.0.0.1 in front of pretend websites, also on
//! 127.0.0.1. A test resolver points the pretend names at them.

use decurion_gate::{serve, Event, Gate, Policy, Resolver};
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

type Events = Arc<Mutex<Vec<Event>>>;

const WAIT: Duration = Duration::from_secs(10);

/// Starts a gate for `sites`. `names` maps each pretend name to a local
/// address; other names fail to resolve.
async fn gate(
    sites: &[&str],
    allow_private: bool,
    names: Vec<(&str, SocketAddr)>,
) -> (SocketAddr, Events) {
    let names: Vec<(String, SocketAddr)> =
        names.into_iter().map(|(n, a)| (n.to_owned(), a)).collect();
    let resolver: Resolver = Arc::new(move |host, _port| {
        let found: Vec<SocketAddr> = names
            .iter()
            .filter(|(n, _)| *n == host)
            .map(|(_, a)| *a)
            .collect();
        Box::pin(async move {
            if found.is_empty() {
                Err(io::Error::new(io::ErrorKind::NotFound, "no such name"))
            } else {
                Ok(found)
            }
        })
    });
    let events: Events = Arc::default();
    let sink = events.clone();
    let policy = Policy {
        sites: sites.iter().map(|s| s.to_string()).collect(),
        allow_private,
    };
    let gate = Gate::new(policy, move |e| {
        if let Ok(mut v) = sink.lock() {
            v.push(e);
        }
    })
    .with_resolver(resolver);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind gate");
    let addr = listener.local_addr().expect("gate addr");
    tokio::spawn(serve(listener, Arc::new(gate)));
    (addr, events)
}

/// A pretend website that sends back whatever it is sent.
async fn echo_site() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind site");
    let addr = listener.local_addr().expect("site addr");
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    addr
}

/// A pretend website that records the first request it gets (head and
/// body, until the client stops sending) and answers `ok`.
async fn http_site() -> (SocketAddr, tokio::sync::oneshot::Receiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind site");
    let addr = listener.local_addr().expect("site addr");
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let Ok((mut s, _)) = listener.accept().await else {
            return;
        };
        let mut got = Vec::new();
        let mut buf = [0u8; 1024];
        // Read until the expected body has arrived.
        while !got.ends_with(b"hello=1") {
            match s.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => got.extend_from_slice(&buf[..n]),
            }
        }
        let _ = s
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await;
        let _ = tx.send(got);
    });
    (addr, rx)
}

/// Sends `request` and reads until the gate closes the connection.
async fn exchange(gate: SocketAddr, request: &[u8]) -> String {
    let mut s = TcpStream::connect(gate).await.expect("connect gate");
    s.write_all(request).await.expect("send");
    let mut out = Vec::new();
    timeout(WAIT, s.read_to_end(&mut out))
        .await
        .expect("gate answered in time")
        .expect("read");
    String::from_utf8_lossy(&out).into_owned()
}

fn blocked(events: &Events) -> Vec<(String, String)> {
    events
        .lock()
        .expect("events")
        .iter()
        .filter_map(|e| match e {
            Event::Blocked { host, reason } => Some((host.clone(), reason.clone())),
            Event::Ready => None,
        })
        .collect()
}

async fn read_exact_n(s: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    timeout(WAIT, s.read_exact(&mut buf))
        .await
        .expect("in time")
        .expect("read");
    buf
}

#[tokio::test]
async fn an_allowed_tunnel_carries_bytes_both_ways() {
    let site = echo_site().await;
    let (gate, events) = gate(&["allowed.test"], true, vec![("api.allowed.test", site)]).await;

    let mut s = TcpStream::connect(gate).await.expect("connect gate");
    // Bytes sent straight after the head must reach the site too.
    s.write_all(
        b"CONNECT api.allowed.test:443 HTTP/1.1\r\nHost: api.allowed.test:443\r\n\r\nearly-",
    )
    .await
    .expect("send");
    let ok = b"HTTP/1.1 200 Connection established\r\n\r\n";
    assert_eq!(read_exact_n(&mut s, ok.len()).await, ok);
    assert_eq!(read_exact_n(&mut s, 6).await, b"early-");
    s.write_all(b"ping").await.expect("send");
    assert_eq!(read_exact_n(&mut s, 4).await, b"ping");
    assert!(blocked(&events).is_empty());
}

#[tokio::test]
async fn an_unlisted_site_is_refused_and_reported_once() {
    let site = echo_site().await;
    let (gate, events) = gate(
        &["allowed.test"],
        true,
        vec![("allowed.test", site), ("evil.test", site)],
    )
    .await;

    for _ in 0..2 {
        let reply = exchange(gate, b"CONNECT evil.test:443 HTTP/1.1\r\n\r\n").await;
        assert!(reply.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{reply}");
        assert!(reply.contains("Content-Type: text/plain\r\n"), "{reply}");
        assert!(reply.contains("Connection: close\r\n"), "{reply}");
        assert!(
            reply.contains("Decurion blocked this: evil.test is not on the list of websites"),
            "{reply}"
        );
    }
    assert_eq!(
        blocked(&events),
        [("evil.test".to_owned(), "not on this app's list".to_owned())]
    );
}

#[tokio::test]
async fn other_ports_are_refused() {
    let site = echo_site().await;
    let (gate, events) = gate(&["allowed.test"], true, vec![("allowed.test", site)]).await;

    let reply = exchange(gate, b"CONNECT allowed.test:8443 HTTP/1.1\r\n\r\n").await;
    assert!(reply.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{reply}");
    let reply = exchange(
        gate,
        b"GET http://allowed.test:8080/ HTTP/1.1\r\nHost: allowed.test\r\n\r\n",
    )
    .await;
    assert!(reply.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{reply}");
    assert_eq!(
        blocked(&events),
        [(
            "allowed.test".to_owned(),
            "uses a port other than 80 or 443".to_owned()
        )]
    );
}

#[tokio::test]
async fn plain_http_is_rewritten_and_forwarded() {
    let (site, got) = http_site().await;
    let (gate, events) = gate(&["allowed.test"], true, vec![("allowed.test", site)]).await;

    let reply = exchange(
        gate,
        b"POST http://allowed.test/form?x=1 HTTP/1.1\r\n\
          Host: allowed.test\r\n\
          Proxy-Connection: keep-alive\r\n\
          Connection: keep-alive\r\n\
          Content-Length: 7\r\n\r\n\
          hello=1",
    )
    .await;
    assert!(reply.starts_with("HTTP/1.1 200 OK\r\n"), "{reply}");
    assert!(reply.ends_with("ok"), "{reply}");
    let got = timeout(WAIT, got)
        .await
        .expect("in time")
        .expect("site saw a request");
    assert_eq!(
        String::from_utf8_lossy(&got),
        "POST /form?x=1 HTTP/1.1\r\nHost: allowed.test\r\nContent-Length: 7\r\nConnection: close\r\n\r\nhello=1"
    );
    assert!(blocked(&events).is_empty());
}

#[tokio::test]
async fn a_listed_name_pointing_home_is_refused() {
    let site = echo_site().await;
    let (gate, events) = gate(&["home.test"], false, vec![("home.test", site)]).await;

    let reply = exchange(gate, b"CONNECT home.test:443 HTTP/1.1\r\n\r\n").await;
    assert!(reply.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{reply}");
    assert!(reply.contains("home network"), "{reply}");
    assert_eq!(
        blocked(&events),
        [(
            "home.test".to_owned(),
            "points at your own PC or home network".to_owned()
        )]
    );
}

#[tokio::test]
async fn ip_addresses_are_refused() {
    let (gate, events) = gate(&["allowed.test"], true, vec![]).await;
    let reply = exchange(gate, b"CONNECT 127.0.0.1:443 HTTP/1.1\r\n\r\n").await;
    assert!(reply.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{reply}");
    assert_eq!(
        blocked(&events),
        [(
            "127.0.0.1".to_owned(),
            "is an IP address, not a website name".to_owned()
        )]
    );
}

#[tokio::test]
async fn a_listed_site_that_cannot_be_found_is_a_502_not_a_block() {
    let (gate, events) = gate(&["allowed.test"], true, vec![]).await;
    let reply = exchange(gate, b"CONNECT allowed.test:443 HTTP/1.1\r\n\r\n").await;
    assert!(reply.starts_with("HTTP/1.1 502 Bad Gateway\r\n"), "{reply}");
    assert!(blocked(&events).is_empty());
}

#[tokio::test]
async fn garbage_gets_a_400() {
    let (gate, events) = gate(&["allowed.test"], true, vec![]).await;
    let reply = exchange(gate, b"GET / HTTP/1.1\r\nHost: allowed.test\r\n\r\n").await;
    assert!(reply.starts_with("HTTP/1.1 400 Bad Request\r\n"), "{reply}");
    let mut big = b"GET http://allowed.test/ HTTP/1.1\r\nX: ".to_vec();
    big.extend(std::iter::repeat_n(b'a', 20_000));
    let reply = exchange(gate, &big).await;
    assert!(reply.starts_with("HTTP/1.1 400 Bad Request\r\n"), "{reply}");
    assert!(blocked(&events).is_empty());
}

#[tokio::test]
async fn the_relay_passes_bytes_to_its_one_target() {
    let upstream = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream");
    let target = upstream.local_addr().expect("addr").to_string();
    tokio::spawn(async move {
        if let Ok((mut s, _)) = upstream.accept().await {
            let mut buf = [0u8; 4];
            if s.read_exact(&mut buf).await.is_ok() {
                let _ = s.write_all(&buf).await;
            }
        }
    });
    let l = TcpListener::bind("127.0.0.1:0").await.expect("bind relay");
    let at = l.local_addr().expect("addr");
    tokio::spawn(decurion_gate::relay(l, target));
    let mut c = TcpStream::connect(at).await.expect("connect");
    c.write_all(b"ping").await.expect("write");
    let mut back = [0u8; 4];
    c.read_exact(&mut back).await.expect("read");
    assert_eq!(&back, b"ping");
}
