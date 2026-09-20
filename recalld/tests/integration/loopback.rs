//! A loopback listener on a port an outgoing connection cannot draw.
//!
//! ⚠⚠ **`bind("127.0.0.1:0")` takes a port from the EPHEMERAL range — and so
//! does the source port of every outgoing connection.** On this machine that
//! range is `net.inet.ip.portrange.first .. last` = 49152-65535, and both ends
//! of a loopback test draw from it. When a client's source port lands on the
//! port it is dialling, BSD sockets complete a simultaneous open: the socket
//! connects to ITSELF, the request is read back as the response, and parsing a
//! request line as a status line reports a bad header rather than anything that
//! sounds like a port collision.
//!
//! That is the shape of the flake three tests have carried between them
//! (#1480, #1630), whose one piece of hard evidence was
//!
//!     Error encountered in a header: Invalid argument (os error 22)
//!
//! on `127.0.0.1:<ephemeral>` — a CONNECTED peer whose reply could not be
//! parsed, which a closed port cannot produce.
//!
//! ⚠ Binding BELOW the ephemeral range removes the collision by construction.
//! It is not a retry, a readiness probe or a timeout: the client cannot be given
//! a source port in a range the kernel does not allocate from.
//!
//! ⚠ **This does not prove the diagnosis on its own** — see the task. What makes
//! it evidence is the ablation: the same sampler that reproduced the failure at
//! a measurable rate, run again with this in place.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

/// Below the ephemeral range and above the well-known ports. Not registered to
/// anything here; a port in use is skipped rather than fought over.
const FIRST: u16 = 20_000;
const LAST: u16 = 32_767;

/// A bound listener on a port outside the ephemeral range.
///
/// # Panics
/// If nothing in the whole window can be bound, which means something far
/// stranger than a busy port.
pub async fn listener() -> tokio::net::TcpListener {
    // ⚠ A varying start, so parallel test binaries do not all race for 20000 and
    // serialise on the same handful of ports.
    let span = u32::from(LAST - FIRST) + 1;
    let jitter = std::process::id().wrapping_mul(2_654_435_761) % span;
    for offset in 0..span {
        let port = FIRST + u16::try_from((jitter + offset) % span).expect("in range");
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
        if let Ok(bound) = tokio::net::TcpListener::bind(addr).await {
            return bound;
        }
    }
    panic!("no free port in {FIRST}..={LAST}");
}
