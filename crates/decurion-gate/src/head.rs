//! Reading and understanding what a box asks the gate for.
//!
//! Clients behind `HTTPS_PROXY` open a tunnel with `CONNECT host:443`; those
//! behind `HTTP_PROXY` send plain requests with the full address in the
//! request line (`GET http://host/path`). Nothing else is needed, so nothing
//! else is accepted.

use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

/// The longest request head the gate reads before giving up.
pub const MAX_HEAD: usize = 16 * 1024;

/// How long a client gets to send its request head.
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(30);

/// What a client asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// A tunnel to `host:port`, for HTTPS.
    Connect { host: String, port: u16 },
    /// A plain HTTP request to `host:port`. `head` is the request head
    /// rewritten for the website itself: origin-form request line, no proxy
    /// or connection headers, and `Connection: close`.
    Http {
        host: String,
        port: u16,
        head: Vec<u8>,
    },
}

impl Request {
    pub fn host(&self) -> &str {
        match self {
            Request::Connect { host, .. } | Request::Http { host, .. } => host,
        }
    }

    pub fn port(&self) -> u16 {
        match self {
            Request::Connect { port, .. } | Request::Http { port, .. } => *port,
        }
    }
}

/// Why a request head could not be read.
#[derive(Debug, PartialEq, Eq)]
pub enum HeadError {
    /// The client hung up before finishing its head.
    Closed,
    /// The head ran past [`MAX_HEAD`].
    TooLarge,
    /// Reading from the client failed.
    Io(std::io::ErrorKind),
}

/// Reads up to and including the blank line that ends a request head.
/// Returns the head and any bytes the client already sent after it, which
/// belong to the website and must be passed on.
pub async fn read_head<R: AsyncRead + Unpin>(r: &mut R) -> Result<(Vec<u8>, Vec<u8>), HeadError> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        let searched = buf.len().saturating_sub(3);
        let n = r
            .read(&mut chunk)
            .await
            .map_err(|e| HeadError::Io(e.kind()))?;
        if n == 0 {
            return Err(HeadError::Closed);
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = find(&buf[searched..], b"\r\n\r\n") {
            let end = searched + i + 4;
            if end > MAX_HEAD {
                return Err(HeadError::TooLarge);
            }
            let rest = buf.split_off(end);
            return Ok((buf, rest));
        }
        if buf.len() >= MAX_HEAD {
            return Err(HeadError::TooLarge);
        }
    }
}

/// Splits on CRLF. A lone CR or LF anywhere is refused: different
/// servers treat those differently, which lets one request be read as two.
fn crlf_lines(body: &[u8]) -> Option<Vec<&[u8]>> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = find(rest, b"\r\n") {
        out.push(&rest[..i]);
        rest = &rest[i + 2..];
    }
    out.push(rest);
    let stray = |l: &&[u8]| l.iter().any(|&b| b == b'\r' || b == b'\n');
    (!out.iter().any(stray)).then_some(out)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Makes sense of a complete request head (as returned by [`read_head`]).
/// `None` means it is neither form the gate accepts.
pub fn parse_head(head: &[u8]) -> Option<Request> {
    let body = head.strip_suffix(b"\r\n\r\n")?;
    let lines = crlf_lines(body)?;
    let mut lines = lines.into_iter();
    let request_line = std::str::from_utf8(lines.next()?).ok()?;
    let mut parts = request_line.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
        return None;
    }
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }

    if method == "CONNECT" {
        let (host, port) = authority(target)?;
        return Some(Request::Connect { host, port: port? });
    }

    let scheme = target.get(..7)?;
    if !scheme.eq_ignore_ascii_case("http://") {
        return None;
    }
    let rest = &target[7..];
    let split = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (host, port) = authority(&rest[..split])?;
    let path = &rest[split..];
    let path = path.split('#').next().unwrap_or("");
    let path = match path.as_bytes().first() {
        None => "/".to_owned(),
        Some(b'?') => format!("/{path}"),
        Some(_) => path.to_owned(),
    };

    let mut out = format!("{method} {path} {version}\r\n").into_bytes();
    for line in lines {
        // Folded header lines are obsolete and a classic way to confuse
        // proxies about where one header ends, so they are refused.
        if line.first().is_some_and(|b| *b == b' ' || *b == b'\t') {
            return None;
        }
        let colon = line.iter().position(|&b| b == b':')?;
        let name = std::str::from_utf8(&line[..colon]).ok()?;
        if name.is_empty() || name.bytes().any(|b| !b.is_ascii_graphic()) {
            return None;
        }
        let name = name.to_ascii_lowercase();
        if name.starts_with("proxy-") || name == "connection" || name == "keep-alive" {
            continue;
        }
        out.extend_from_slice(line);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"Connection: close\r\n\r\n");
    Some(Request::Http {
        host,
        port: port.unwrap_or(80),
        head: out,
    })
}

/// Splits `host[:port]` or `[v6]:port`. The host comes back lower-cased with
/// any trailing dot removed; the port is `None` when absent. Anything that
/// is not plainly a name or an address is refused.
fn authority(s: &str) -> Option<(String, Option<u16>)> {
    let (host, port) = if let Some(inner) = s.strip_prefix('[') {
        let (v6, after) = inner.split_once(']')?;
        if v6.is_empty()
            || !v6
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
        {
            return None;
        }
        let port = match after {
            "" => None,
            p => Some(p.strip_prefix(':')?),
        };
        (format!("[{v6}]"), port)
    } else {
        match s.split_once(':') {
            Some((h, p)) => (h.to_owned(), Some(p)),
            None => (s.to_owned(), None),
        }
    };
    let port = match port {
        None => None,
        Some(p) => {
            if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some(p.parse::<u16>().ok().filter(|&p| p != 0)?)
        }
    };
    let mut host = host.to_ascii_lowercase();
    if host.ends_with('.') {
        host.pop();
    }
    let name_ok = host
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_'));
    if host.is_empty() || host.len() > 253 || !(name_ok || host.starts_with('[')) {
        return None;
    }
    Some((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http_head(req: &str) -> String {
        match parse_head(req.as_bytes()) {
            Some(Request::Http { head, .. }) => String::from_utf8(head).expect("utf-8"),
            other => panic!("not plain HTTP: {other:?}"),
        }
    }

    #[test]
    fn connect_is_understood() {
        assert_eq!(
            parse_head(b"CONNECT API.Example.com.:443 HTTP/1.1\r\nHost: x\r\n\r\n"),
            Some(Request::Connect {
                host: "api.example.com".into(),
                port: 443
            })
        );
        assert_eq!(
            parse_head(b"CONNECT [::1]:443 HTTP/1.1\r\n\r\n"),
            Some(Request::Connect {
                host: "[::1]".into(),
                port: 443
            })
        );
        assert_eq!(
            parse_head(b"CONNECT 10.0.0.1:22 HTTP/1.0\r\n\r\n"),
            Some(Request::Connect {
                host: "10.0.0.1".into(),
                port: 22
            })
        );
    }

    #[test]
    fn connect_needs_a_port() {
        assert_eq!(parse_head(b"CONNECT example.com HTTP/1.1\r\n\r\n"), None);
        assert_eq!(parse_head(b"CONNECT example.com: HTTP/1.1\r\n\r\n"), None);
        assert_eq!(parse_head(b"CONNECT example.com:0 HTTP/1.1\r\n\r\n"), None);
        assert_eq!(
            parse_head(b"CONNECT example.com:99999 HTTP/1.1\r\n\r\n"),
            None
        );
        assert_eq!(parse_head(b"CONNECT ::1:443 HTTP/1.1\r\n\r\n"), None);
    }

    #[test]
    fn plain_http_is_rewritten_for_the_website() {
        let req = "GET http://Example.com/a/b?c=1#frag HTTP/1.1\r\n\
                   Host: example.com\r\n\
                   Proxy-Connection: keep-alive\r\n\
                   Proxy-Authorization: Basic eA==\r\n\
                   Connection: keep-alive\r\n\
                   Keep-Alive: timeout=5\r\n\
                   Accept: */*\r\n\r\n";
        assert_eq!(
            http_head(req),
            "GET /a/b?c=1 HTTP/1.1\r\nHost: example.com\r\nAccept: */*\r\nConnection: close\r\n\r\n"
        );
        match parse_head(req.as_bytes()) {
            Some(r) => assert_eq!((r.host(), r.port()), ("example.com", 80)),
            None => panic!("refused"),
        }
    }

    #[test]
    fn plain_http_paths_and_ports() {
        assert!(
            http_head("GET http://example.com HTTP/1.1\r\n\r\n").starts_with("GET / HTTP/1.1\r\n")
        );
        assert!(http_head("GET http://example.com?q HTTP/1.1\r\n\r\n")
            .starts_with("GET /?q HTTP/1.1\r\n"));
        match parse_head(b"POST http://example.com:8080/x HTTP/1.1\r\n\r\n") {
            Some(r) => assert_eq!(r.port(), 8080),
            None => panic!("refused"),
        }
    }

    #[test]
    fn garbage_and_other_forms_are_refused() {
        for bad in [
            &b"\r\n\r\n"[..],
            b"hello\r\n\r\n",
            b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n",
            b"GET https://example.com/ HTTP/1.1\r\n\r\n",
            b"GET ftp://example.com/ HTTP/1.1\r\n\r\n",
            b"GET http://user@example.com/ HTTP/1.1\r\n\r\n",
            b"GET http://exa%6dple.com/ HTTP/1.1\r\n\r\n",
            b"GET http:///x HTTP/1.1\r\n\r\n",
            b"GET  http://example.com/ HTTP/1.1\r\n\r\n",
            b"GET http://example.com/ HTTP/2\r\n\r\n",
            b"get http://example.com/ HTTP/1.1\r\n\r\n",
            b"GET http://example.com/ HTTP/1.1\nHost: x\n\n",
            b"GET http://example.com/ HTTP/1.1\r\nHost: x\r\n folded\r\n\r\n",
            b"GET http://example.com/ HTTP/1.1\r\nno colon\r\n\r\n",
            b"GET http://example.com/ HTTP/1.1\r\nBad Name: x\r\n\r\n",
            b"\xff\xfe CONNECT\r\n\r\n",
            b"CONNECT example.com:443 HTTP/1.1\r\n",
        ] {
            assert_eq!(parse_head(bad), None, "{}", String::from_utf8_lossy(bad));
        }
    }

    #[tokio::test]
    async fn head_and_early_body_are_split() {
        let mut input: &[u8] = b"CONNECT a.com:443 HTTP/1.1\r\n\r\n\x16\x03\x01hello";
        let (head, rest) = read_head(&mut input).await.expect("head");
        assert_eq!(head, b"CONNECT a.com:443 HTTP/1.1\r\n\r\n");
        assert_eq!(rest, b"\x16\x03\x01hello");
    }

    #[tokio::test]
    async fn oversized_and_unfinished_heads_are_refused() {
        let mut big = b"GET http://a.com/ HTTP/1.1\r\n".to_vec();
        big.extend(std::iter::repeat_n(b'a', MAX_HEAD));
        big.extend_from_slice(b"\r\n\r\n");
        assert_eq!(read_head(&mut &big[..]).await, Err(HeadError::TooLarge));

        // Just over the limit, ending in the same read as the terminator.
        let mut edge = vec![b'a'; MAX_HEAD - 3];
        edge.extend_from_slice(b"\r\n\r\n");
        assert_eq!(read_head(&mut &edge[..]).await, Err(HeadError::TooLarge));

        let mut short: &[u8] = b"CONNECT a.com:443 HTTP/1.1\r\n";
        assert_eq!(read_head(&mut short).await, Err(HeadError::Closed));
    }
}
