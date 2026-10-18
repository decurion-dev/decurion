//! The gate: the only way out of an agent's box.
//!
//! Each box sits on its own Docker network with no route to anywhere. The
//! gate sits on that network and on the normal one, and the box is told to
//! use it as its web proxy. It lets through only the websites on the app's
//! list, only on the usual web ports, and never to the owner's own PC or home
//! network, whatever a website's DNS says. Everything it refuses is reported
//! as one JSON line on stdout so the runner can tell the owner.

pub mod addr;
pub mod head;

use decurion_proto::sites;
use head::{HeadError, Request};
use std::collections::HashSet;
use std::future::Future;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::time::timeout;

/// The port the gate listens on inside its container.
pub const PORT: u16 = 3128;

/// At most this many boxes' connections are served at once; the rest wait.
pub const MAX_CONNECTIONS: usize = 256;

/// How long to wait for a website to answer a connection or a DNS lookup.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How many refused hosts are remembered so each is reported once. Past
/// this the memory is cleared rather than letting it grow without end.
pub const MAX_REMEMBERED: usize = 4096;

const CARRY_ON: &str = "Carry on without it, and say in your summary if the work needs it.";

/// What the gate lets through.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// Site names as `sites::normalize` produces them. Each covers itself
    /// and every address under it.
    pub sites: Vec<String>,
    /// Lets connections reach private addresses. Only for tests, whose
    /// pretend websites run on 127.0.0.1; the gate binary never sets it.
    pub allow_private: bool,
}

impl Policy {
    /// Reads the comma-separated list in `DECURION_SITES`. Entries that are
    /// not site names are left out and returned, so a mistake such as `com`
    /// can never cover a whole ending.
    pub fn from_list(list: &str) -> (Policy, Vec<String>) {
        let mut sites = Vec::new();
        let mut rejected = Vec::new();
        for entry in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let lower = entry.to_ascii_lowercase();
            if sites::is_site_name(&lower) {
                if !sites.contains(&lower) {
                    sites.push(lower);
                }
            } else {
                rejected.push(entry.to_owned());
            }
        }
        (
            Policy {
                sites,
                allow_private: false,
            },
            rejected,
        )
    }
}

/// Why the gate refused a request. Each comes with plain words the owner
/// sees and a message the agent reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The request named an IP address instead of a website.
    IpAddress,
    /// The website is not on this app's list.
    NotListed,
    /// A port other than 443 for tunnels or 80 for plain HTTP.
    Port,
    /// The name leads to a private address: the owner's PC, their home
    /// network, Docker's networks or Tailscale.
    HomeNetwork,
}

impl Refusal {
    /// The `reason` in the blocked event, written for the owner.
    pub fn reason(self) -> &'static str {
        match self {
            Refusal::IpAddress => "is an IP address, not a website name",
            Refusal::NotListed => "not on this app's list",
            Refusal::Port => "uses a port other than 80 or 443",
            Refusal::HomeNetwork => "points at your own PC or home network",
        }
    }

    /// The body of the 403 reply, written for the agent.
    pub fn message(self, host: &str, port: u16) -> String {
        let why = match self {
            Refusal::IpAddress => format!(
                "{host} is an IP address, not a website name. Agents may only use websites on this app's list, by name."
            ),
            Refusal::NotListed => {
                format!("{host} is not on the list of websites this app's agents may use.")
            }
            Refusal::Port => format!(
                "{host} was asked for on port {port}. Agents may only use port 443 for https and port 80 for http."
            ),
            Refusal::HomeNetwork => format!(
                "{host} points at the owner's own PC or home network, which agents may never reach."
            ),
        };
        format!("Decurion blocked this: {why} {CARRY_ON}\n")
    }
}

/// Whether a request may go ahead, judged before any DNS lookup.
/// `tunnel` is true for CONNECT (HTTPS), false for plain HTTP.
pub fn decide(policy: &Policy, host: &str, port: u16, tunnel: bool) -> Result<(), Refusal> {
    if addr::is_ip_literal(host) {
        return Err(Refusal::IpAddress);
    }
    if !policy.sites.iter().any(|s| sites::covers(s, host)) {
        return Err(Refusal::NotListed);
    }
    let wanted = if tunnel { 443 } else { 80 };
    if port != wanted {
        return Err(Refusal::Port);
    }
    Ok(())
}

/// Keeps only the public addresses a name resolved to. If none are left the
/// name points somewhere private and is refused.
pub fn public_only(
    addrs: Vec<SocketAddr>,
    allow_private: bool,
) -> Result<Vec<SocketAddr>, Refusal> {
    let public: Vec<SocketAddr> = addrs
        .into_iter()
        .filter(|a| allow_private || addr::is_public(a.ip()))
        .collect();
    if public.is_empty() {
        Err(Refusal::HomeNetwork)
    } else {
        Ok(public)
    }
}

/// One line the gate reports on stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The gate is listening.
    Ready,
    /// The gate refused a host, for the given reason (see
    /// [`Refusal::reason`]).
    Blocked { host: String, reason: String },
}

impl Event {
    /// The event as one line of JSON, without the newline:
    /// `{"event":"ready"}` or
    /// `{"event":"blocked","host":"…","reason":"…"}`.
    pub fn to_json(&self) -> String {
        match self {
            Event::Ready => r#"{"event":"ready"}"#.to_owned(),
            Event::Blocked { host, reason } => format!(
                r#"{{"event":"blocked","host":{},"reason":{}}}"#,
                serde_json::Value::from(host.as_str()),
                serde_json::Value::from(reason.as_str())
            ),
        }
    }
}

/// Prints an event on stdout. The stdout lock keeps lines from different
/// connections from interleaving, and the flush gets each line to the
/// runner straight away.
pub fn print_event(event: Event) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{}", event.to_json());
    let _ = out.flush();
}

/// Looks up a website's addresses.
pub type Resolver = Arc<
    dyn Fn(String, u16) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>>
        + Send
        + Sync,
>;

/// The ordinary DNS lookup, through the container's resolver.
pub fn system_resolver() -> Resolver {
    Arc::new(|host, port| {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((host.as_str(), port))
                .await?
                .collect())
        })
    })
}

/// A running gate's settings and memory.
pub struct Gate {
    policy: Policy,
    resolver: Resolver,
    events: Box<dyn Fn(Event) + Send + Sync>,
    reported: Mutex<HashSet<String>>,
}

impl Gate {
    /// A gate using the system resolver that hands its events to `events`
    /// (the binary passes [`print_event`]).
    pub fn new(policy: Policy, events: impl Fn(Event) + Send + Sync + 'static) -> Gate {
        Gate {
            policy,
            resolver: system_resolver(),
            events: Box::new(events),
            reported: Mutex::new(HashSet::new()),
        }
    }

    /// Swaps the DNS lookup, so tests can point names at local servers.
    pub fn with_resolver(mut self, resolver: Resolver) -> Gate {
        self.resolver = resolver;
        self
    }

    /// Reports a refused host, once per gate run.
    fn report(&self, host: &str, refusal: Refusal) {
        let host: String = host.chars().take(253).collect();
        {
            let Ok(mut seen) = self.reported.lock() else {
                return;
            };
            if seen.contains(&host) {
                return;
            }
            if seen.len() >= MAX_REMEMBERED {
                seen.clear();
            }
            seen.insert(host.clone());
        }
        (self.events)(Event::Blocked {
            host,
            reason: refusal.reason().to_owned(),
        });
    }
}

/// Serves connections until the listener fails. Does not print the ready
/// line; the caller does that once the listener is bound.
/// Port where a pay-per-use box reaches the gatekeeper through the gate.
pub const RELAY_PORT: u16 = 3129;

/// Passes every connection on `listener` straight through to `target`, the
/// gatekeeper on the PC that holds the pay-per-use key. It is the one
/// address on the PC a box can reach, and it is fixed by the runner, never
/// by the box. Unlike sites on the list, it is expected to be private.
pub async fn relay(listener: TcpListener, target: String) -> io::Result<()> {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let Ok(slot) = slots.clone().acquire_owned().await else {
            return Ok(());
        };
        let mut client = match listener.accept().await {
            Ok((client, _)) => client,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let target = target.clone();
        tokio::spawn(async move {
            if let Ok(Ok(mut upstream)) =
                timeout(CONNECT_TIMEOUT, TcpStream::connect(target.as_str())).await
            {
                let _ = client.set_nodelay(true);
                let _ = upstream.set_nodelay(true);
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            }
            drop(slot);
        });
    }
}

pub async fn serve(listener: TcpListener, gate: Arc<Gate>) -> io::Result<()> {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let Ok(slot) = slots.clone().acquire_owned().await else {
            return Ok(());
        };
        let client = match listener.accept().await {
            Ok((client, _)) => client,
            // Usually out of file handles; give connections a moment to
            // finish rather than spinning.
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let gate = gate.clone();
        tokio::spawn(async move {
            handle(&gate, client).await;
            drop(slot);
        });
    }
}

async fn handle(gate: &Gate, mut client: TcpStream) {
    let _ = client.set_nodelay(true);
    let (head, early) = match timeout(head::HEAD_TIMEOUT, head::read_head(&mut client)).await {
        Ok(Ok(read)) => read,
        Ok(Err(HeadError::TooLarge)) => {
            return bad_request(client).await;
        }
        Ok(Err(_)) | Err(_) => return,
    };
    let Some(request) = head::parse_head(&head) else {
        return bad_request(client).await;
    };
    let (host, port) = (request.host().to_owned(), request.port());
    let tunnel = matches!(request, Request::Connect { .. });

    if let Err(refusal) = decide(&gate.policy, &host, port, tunnel) {
        return refuse(gate, client, &host, port, refusal).await;
    }
    let addrs = match timeout(CONNECT_TIMEOUT, (gate.resolver)(host.clone(), port)).await {
        Ok(Ok(addrs)) if !addrs.is_empty() => addrs,
        _ => return unreachable(client, &host).await,
    };
    let addrs = match public_only(addrs, gate.policy.allow_private) {
        Ok(addrs) => addrs,
        Err(refusal) => return refuse(gate, client, &host, port, refusal).await,
    };
    let Some(mut upstream) = connect_any(&addrs).await else {
        return unreachable(client, &host).await;
    };

    let first = match request {
        Request::Connect { .. } => {
            if client
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .is_err()
            {
                return;
            }
            early
        }
        Request::Http { head, .. } => [head, early].concat(),
    };
    if upstream.write_all(&first).await.is_err() {
        return;
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
}

/// Tries each address in turn, as browsers do when a site has several.
async fn connect_any(addrs: &[SocketAddr]) -> Option<TcpStream> {
    for a in addrs {
        if let Ok(Ok(s)) = timeout(CONNECT_TIMEOUT, TcpStream::connect(a)).await {
            let _ = s.set_nodelay(true);
            return Some(s);
        }
    }
    None
}

async fn refuse(gate: &Gate, client: TcpStream, host: &str, port: u16, refusal: Refusal) {
    // Reported before replying, so whoever sees the 403 can already find
    // the event.
    gate.report(host, refusal);
    reply(client, "403 Forbidden", &refusal.message(host, port)).await;
}

async fn unreachable(client: TcpStream, host: &str) {
    let body = format!(
        "Decurion could not reach {host}. It is on this app's list, but it could not be found or did not answer. Try again later, or carry on without it.\n"
    );
    reply(client, "502 Bad Gateway", &body).await;
}

async fn bad_request(client: TcpStream) {
    let body = "Decurion's gate did not understand this request. It handles http:// and https:// addresses only.\n";
    reply(client, "400 Bad Request", body).await;
}

async fn reply(mut client: TcpStream, status: &str, body: &str) {
    let msg = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = client.write_all(msg.as_bytes()).await;
    let _ = client.shutdown().await;
    // Closing with unread bytes waiting makes the system reset the
    // connection, which can wipe out the reply before the client reads it.
    // So read and discard what is left, for a moment.
    let mut sink = [0u8; 4096];
    let _ = timeout(Duration::from_secs(1), async {
        let mut left = 64 * 1024;
        while left > 0 {
            match client.read(&mut sink).await {
                Ok(0) | Err(_) => break,
                Ok(n) => left -= n.min(left),
            }
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(sites: &[&str]) -> Policy {
        Policy {
            sites: sites.iter().map(|s| s.to_string()).collect(),
            allow_private: false,
        }
    }

    #[test]
    fn listed_sites_on_the_right_port_go_ahead() {
        let p = policy(&["example.com", "pypi.org"]);
        assert_eq!(decide(&p, "example.com", 443, true), Ok(()));
        assert_eq!(decide(&p, "api.example.com", 443, true), Ok(()));
        assert_eq!(decide(&p, "pypi.org", 80, false), Ok(()));
    }

    #[test]
    fn everything_else_is_refused_for_a_reason() {
        let p = policy(&["example.com"]);
        assert_eq!(decide(&p, "evil.org", 443, true), Err(Refusal::NotListed));
        assert_eq!(
            decide(&p, "evilexample.com", 443, true),
            Err(Refusal::NotListed)
        );
        assert_eq!(
            decide(&p, "example.com.evil.org", 443, true),
            Err(Refusal::NotListed)
        );
        assert_eq!(decide(&p, "example.com", 22, true), Err(Refusal::Port));
        assert_eq!(decide(&p, "example.com", 80, true), Err(Refusal::Port));
        assert_eq!(decide(&p, "example.com", 443, false), Err(Refusal::Port));
        assert_eq!(
            decide(&p, "93.184.215.14", 443, true),
            Err(Refusal::IpAddress)
        );
        assert_eq!(decide(&p, "[::1]", 443, true), Err(Refusal::IpAddress));
        assert_eq!(
            decide(&policy(&[]), "example.com", 443, true),
            Err(Refusal::NotListed)
        );
    }

    #[test]
    fn private_addresses_are_dropped() {
        let a = |s: &str| s.parse::<SocketAddr>().expect("addr");
        assert_eq!(
            public_only(vec![a("192.168.1.2:443"), a("1.1.1.1:443")], false),
            Ok(vec![a("1.1.1.1:443")])
        );
        assert_eq!(
            public_only(vec![a("127.0.0.1:443"), a("[::ffff:10.0.0.1]:443")], false),
            Err(Refusal::HomeNetwork)
        );
        assert_eq!(
            public_only(vec![a("127.0.0.1:443")], true),
            Ok(vec![a("127.0.0.1:443")])
        );
    }

    #[test]
    fn the_site_list_is_read_carefully() {
        let (p, rejected) = Policy::from_list(" example.com,Pypi.org,,com,10.0.0.1,example.com ");
        assert_eq!(p.sites, ["example.com", "pypi.org"]);
        assert_eq!(rejected, ["com", "10.0.0.1"]);
        assert!(!p.allow_private);
    }

    #[test]
    fn events_are_one_line_of_json() {
        assert_eq!(Event::Ready.to_json(), r#"{"event":"ready"}"#);
        let e = Event::Blocked {
            host: "a\"b.com".into(),
            reason: Refusal::NotListed.reason().into(),
        };
        assert_eq!(
            e.to_json(),
            r#"{"event":"blocked","host":"a\"b.com","reason":"not on this app's list"}"#
        );
    }

    #[test]
    fn each_host_is_reported_once() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let gate = Gate::new(policy(&[]), move |e| {
            if let Ok(mut v) = sink.lock() {
                v.push(e);
            }
        });
        gate.report("a.com", Refusal::NotListed);
        gate.report("a.com", Refusal::Port);
        gate.report("b.com", Refusal::NotListed);
        assert_eq!(seen.lock().map(|v| v.len()).unwrap_or(0), 2);
    }

    #[test]
    fn messages_name_the_host_and_say_carry_on() {
        let m = Refusal::NotListed.message("evil.org", 443);
        assert!(m.starts_with("Decurion blocked this: evil.org is not on the list"));
        assert!(m.contains(CARRY_ON));
        assert!(Refusal::Port.message("a.com", 8443).contains("8443"));
    }
}
