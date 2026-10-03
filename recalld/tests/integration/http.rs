//! HTTP for tests, over a bare socket: one request per connection, read to its
//! end.
//!
//! ⚠ Not ureq: after an empty body it returns its socket to a pool with a call
//! that fails (EINVAL) once the server has closed it, and panics rather than
//! return the error (#1480).

use std::fmt::Write as _;
use std::io::{Read, Write};

/// Send one request to `addr` and return the status and the body.
pub async fn request(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> (u16, String) {
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (name, value) in headers {
        let _ = write!(head, "{name}: {value}\r\n");
    }
    let body = body.unwrap_or("").to_owned();
    let _ = write!(head, "Content-Length: {}\r\n\r\n", body.len());
    let addr = addr.to_owned();
    let reply = tokio::task::spawn_blocking(move || {
        let mut stream = std::net::TcpStream::connect(&addr).expect("connect");
        // One write: a body arriving after the server has answered from the
        // head alone (a 404 for an unmounted route) is unread when it closes,
        // and the kernel answers that with a reset.
        stream
            .write_all(format!("{head}{body}").as_bytes())
            .expect("request");
        let mut reply = Vec::new();
        if let Err(e) = stream.read_to_end(&mut reply) {
            // The same reset can still race the read. The answer has already
            // arrived once its head is complete; a reset before that is a failure.
            let answered = reply.windows(4).any(|w| w == b"\r\n\r\n");
            assert!(
                e.kind() == std::io::ErrorKind::ConnectionReset && answered,
                "reading the reply: {e}"
            );
        }
        String::from_utf8(reply).expect("a utf-8 reply")
    })
    .await
    .expect("request");
    let (head, rest) = reply.split_once("\r\n\r\n").expect("a head and a body");
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("a status line");
    let chunked = head.lines().any(|l| {
        l.to_ascii_lowercase()
            .starts_with("transfer-encoding: chunked")
    });
    (
        status,
        if chunked {
            dechunk(rest)
        } else {
            rest.to_owned()
        },
    )
}

/// A chunked body's data, its size lines dropped.
fn dechunk(mut rest: &str) -> String {
    let mut out = String::new();
    while let Some((size, after)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size.trim(), 16).expect("a chunk size");
        if size == 0 {
            break;
        }
        out.push_str(&after[..size]);
        rest = &after[size + 2..];
    }
    out
}

/// A query value spelled for a URL: an RFC 3339 `+` would arrive as a space.
pub fn query(value: &str) -> String {
    form_urlencoded::byte_serialize(value.as_bytes()).collect()
}
