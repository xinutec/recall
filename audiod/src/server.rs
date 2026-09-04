//! Single-port audio ingest server. Port of `src/recall/stream_server.py`.
//!
//! Every phone shares ONE port; the handshake carries identity, not the port.
//! A device opens a connection, sends a one-line handshake announcing its id
//! and PCM format, then streams raw PCM. The server reads only the handshake,
//! then pumps the socket into an ffmpeg segmenter — so ffmpeg does all the
//! audio, gap-free. The measured stream is the liveness signal (the marker is
//! refreshed only while real signal arrives), so there is no separate
//! heartbeat — and no way for a silent stream to read as recording.

use crate::meter::{SILENCE_PEAK, StreamMeter};
use crate::pause;
use crate::rebase::{connection_offset, rebase_segment_names, segment_glob};
use crate::segmenter::{CaptureConfig, build_segment_argv, segment_output_pattern};
use crate::store;
use crate::wire::{Handshake, HandshakeError, read_handshake};
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A mic stream is CONTINUOUS — 48 kHz * 2 bytes flows even in a silent room —
/// so no data for this long means the peer is gone, and that is a far stronger
/// signal than TCP keepalive, which never probes a connection the kernel still
/// believes is fine. 15 s tolerates a brief Wi-Fi stall.
const READ_TIMEOUT: Duration = Duration::from_secs(15);
const READ_CHUNK_BYTES: usize = 65536;
/// How often the accept loop re-checks the global pause.
const PAUSE_POLL: Duration = Duration::from_secs(2);
/// How long a nonblocking accept sleeps when nothing is waiting.
const ACCEPT_IDLE: Duration = Duration::from_millis(250);
/// How often the pump sweeps for closed segments to rebase. Cheap (one
/// listdir), and well inside the worker's 120 s min-age indexing guard.
const REBASE_SWEEP: Duration = Duration::from_secs(10);

fn unix_seconds(time: SystemTime) -> f64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Refresh the source's liveness marker — call only on measured signal.
fn mark_alive(source_dir: &Path) {
    let _ = std::fs::write(source_dir.join(".alive"), b"");
}

/// The segment file this connection last finalised: the newest one touched
/// since the connection opened. `None` when the connection wrote no file at
/// all — naming an older file would blame the wrong window.
fn flushed_segment(out_dir: &Path, source_id: &str, since: SystemTime) -> Option<(String, u64)> {
    segment_glob(out_dir, source_id)
        .into_iter()
        .filter_map(|path| {
            let meta = path.metadata().ok()?;
            let mtime = meta.modified().ok()?;
            if mtime < since {
                return None;
            }
            let name = path.file_name()?.to_str()?.to_owned();
            Some((mtime, name, meta.len()))
        })
        .max_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)))
        .map(|(_, name, size)| (name, size))
}

/// The socket -> segmenter pump, and what it learned on the way. A struct
/// rather than a function returning its findings, because the caller's
/// cleanup files the disconnect record even when the pump exits early — and
/// on that path `first_byte` is still evidence.
struct Pump<'a> {
    stream: &'a TcpStream,
    stdin: Option<std::process::ChildStdin>,
    out_dir: &'a Path,
    meter: &'a mut StreamMeter,
    source_id: &'a str,
    /// The phone's announced capture epoch; `None` = arrival-stamp.
    epoch: Option<f64>,
    /// Set by the serve loop before it closes this socket for a pause, so the
    /// disconnect record can tell "the pause dropped it" from "the peer left".
    dropped_by_pause: &'a AtomicBool,
    /// Why the stream ended, for the disconnect record. The endings are not
    /// the same event: a phone walking out of range and a pause dropping the
    /// stream both just stop.
    ended: String,
    first_byte: Option<f64>,
    /// capture-minus-arrival for this connection, fixed at the first byte.
    offset_s: Option<f64>,
    /// Segment names already rebased (or created by a rebase) this connection.
    rebased: HashSet<String>,
    next_sweep: Option<Instant>,
}

impl Pump<'_> {
    fn run(&mut self) {
        use std::io::Read;
        let mut buf = vec![0u8; READ_CHUNK_BYTES];
        let mut sock = self.stream;
        loop {
            let n = match sock.read(&mut buf) {
                Ok(0) => {
                    self.ended = if self.dropped_by_pause.load(Ordering::Relaxed) {
                        "closed locally (pause)".into()
                    } else {
                        "device disconnected".into()
                    };
                    return;
                }
                Ok(n) => n,
                Err(err)
                    if matches!(
                        err.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    self.ended = format!("no data for {}s — peer gone", READ_TIMEOUT.as_secs());
                    tracing::warn!(source = self.source_id, "ingest: stopped sending");
                    return;
                }
                Err(err) => {
                    // The serve loop closes an active socket to drop the stream
                    // when capture pauses; an errno on the closed fd is how the
                    // reader is told. Expected, so it finalises like any other
                    // disconnect instead of escaping as a crash.
                    self.ended = format!("closed locally ({err})");
                    return;
                }
            };
            let data = &buf[..n];
            // The archive comes first; meter after.
            let stdin = self.stdin.as_mut().expect("stdin open while pumping");
            if let Err(err) = stdin.write_all(data) {
                self.ended = format!("segmenter pipe broke ({err})");
                tracing::error!(source = self.source_id, error = %err, "ingest: segmenter died mid-stream");
                return;
            }
            if self.first_byte.is_none() {
                let now = unix_seconds(SystemTime::now());
                self.first_byte = Some(now);
                self.offset_s = connection_offset(self.epoch, now);
            }
            self.maybe_rebase(false);
            let heard = self.meter.first_audible_byte.is_some();
            // Liveness: refresh the marker only when the chunk carries real
            // signal — "active" must mean recording. A connected phone
            // streaming digital silence (the pixel9 dead path) reads idle, so
            // nobody speaks trusting a dot the audio can't back.
            if self.meter.feed(data) >= SILENCE_PEAK {
                mark_alive(self.out_dir);
            }
            if !heard && let Some(seconds) = self.meter.first_audible_s() {
                tracing::info!(
                    source = self.source_id,
                    at_s = format!("{seconds:.2}"),
                    "ingest: first audible sample"
                );
            }
        }
    }

    /// Rename this connection's closed segments to capture time. Rides the
    /// pump loop rather than a thread — one fewer thing to stop. `finished`
    /// runs once after the segmenter has exited: every segment is closed then,
    /// so even the newest name is safe to move — without it the connection's
    /// LAST segment would stay arrival-stamped forever.
    fn maybe_rebase(&mut self, finished: bool) {
        let (Some(offset_s), Some(first_byte)) = (self.offset_s, self.first_byte) else {
            return;
        };
        let now = Instant::now();
        if !finished && self.next_sweep.is_some_and(|next| now < next) {
            return;
        }
        self.next_sweep = Some(now + REBASE_SWEEP);
        // 2 s slack: ffmpeg stamps the first segment by strftime (whole
        // seconds), which can floor to just before the measured first-byte
        // instant. The prior-connection exclusion only needs coarse precision.
        let since = DateTime::<Utc>::from_timestamp_micros(((first_byte - 2.0) * 1e6) as i64)
            .unwrap_or_default();
        rebase_segment_names(
            self.out_dir,
            self.source_id,
            offset_s,
            &mut self.rebased,
            since,
            finished,
        );
    }
}

/// Serve one device connection: read its handshake, register it, then pump its
/// raw-PCM stream into a segmenter child. The kernel's TCP receive buffer
/// absorbs any pause in the pump, so a momentary stall can't lose audio.
/// Returns when the device disconnects.
///
/// Every connection leaves durable evidence (`capture_events`): an
/// `ingest_connect` on open, and an `ingest_disconnect` on close carrying what the
/// device actually sent. That record is what tells a stream of digital silence
/// from no stream at all when speech goes missing.
pub fn handle_connection(
    stream: &TcpStream,
    root: &Path,
    config: &CaptureConfig,
    dropped_by_pause: &AtomicBool,
) {
    // Bound every read on this socket, the handshake included — a peer that
    // connects and then says nothing (a port scanner, a phone that died
    // between connect and handshake) must not hold a thread for good.
    if stream.set_read_timeout(Some(READ_TIMEOUT)).is_err() {
        return;
    }
    let mut reader = stream;
    let handshake = match read_handshake(&mut reader) {
        Ok(handshake) => handshake,
        Err(HandshakeError::Io(_)) => {
            tracing::warn!("ingest: no handshake within {}s", READ_TIMEOUT.as_secs());
            return;
        }
        Err(_) => {
            tracing::warn!("ingest: malformed handshake, dropping connection");
            return;
        }
    };
    serve_stream(stream, root, config, dropped_by_pause, &handshake);
}

fn serve_stream(
    stream: &TcpStream,
    root: &Path,
    config: &CaptureConfig,
    dropped_by_pause: &AtomicBool,
    handshake: &Handshake,
) {
    let source_id = handshake.source_id.as_str();
    store::register_source(root, source_id);
    let out_dir = root.join(source_id);
    if let Err(err) = std::fs::create_dir_all(&out_dir) {
        tracing::error!(source = source_id, error = %err, "ingest: cannot create source directory");
        return;
    }
    let seg = CaptureConfig {
        sample_rate: handshake.sample_rate,
        channels: handshake.channels,
        ..config.clone()
    };
    let pattern = segment_output_pattern(root, source_id, seg.codec.container_ext());
    tracing::info!(source = source_id, "ingest: connected");
    store::add_capture_event(
        root,
        store::KIND_INGEST_CONNECT,
        Utc::now(),
        source_id,
        None,
    );
    let connected = SystemTime::now();
    let mut meter = StreamMeter::new(handshake.sample_rate, handshake.channels);
    let mut child = match std::process::Command::new(&seg.program)
        .args(build_segment_argv(&seg, &pattern))
        .env("TZ", "UTC") // segment names embed UTC wall-clock
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            tracing::error!(source = source_id, error = %err, "ingest: cannot start segmenter");
            return;
        }
    };
    let stdin = child.stdin.take().expect("piped stdin");
    let mut pump = Pump {
        stream,
        stdin: Some(stdin),
        out_dir: &out_dir,
        meter: &mut meter,
        source_id,
        epoch: handshake.epoch,
        dropped_by_pause,
        ended: "unknown".into(),
        first_byte: None,
        offset_s: None,
        rebased: HashSet::new(),
        next_sweep: None,
    };
    pump.run();
    let first_byte = pump.first_byte;
    drop(pump.stdin.take()); // EOF -> the segmenter finalises the current segment
    let _ = child.wait();
    let flushed = flushed_segment(&out_dir, source_id, connected);
    // Every segment is closed now: give the connection's LAST one its
    // capture-time name too. After flushed_segment, whose stats keep the
    // segmenter's own (arrival) name for the disconnect record.
    pump.maybe_rebase(true);
    let ended = std::mem::take(&mut pump.ended);
    drop(pump); // releases the meter borrow for the stats below
    let connected_s = unix_seconds(connected);
    let stats = serde_json::json!({
        "seconds": (unix_seconds(SystemTime::now()) - connected_s).max(0.0).round_to(1),
        "bytes": meter.bytes_total,
        "peak_db": meter.peak_db(),
        "first_byte_s": first_byte.map(|t| (t - connected_s).round_to(2)),
        "first_audible_s": meter.first_audible_s().map(|s| s.round_to(2)),
        "flushed": flushed.as_ref().map(|(name, _)| name.clone()),
        "flushed_bytes": flushed.as_ref().map(|(_, size)| *size),
        "ended": ended,
    })
    .to_string();
    store::add_capture_event(
        root,
        store::KIND_INGEST_DISCONNECT,
        Utc::now(),
        source_id,
        Some(&stats),
    );
    tracing::info!(source = source_id, stats = %stats, "ingest: disconnected");
}

/// Round-to-decimals for the disconnect stats, matching Python's round(x, n)
/// closely enough for telemetry (ties differ; nothing reads them that finely).
trait RoundTo {
    fn round_to(self, decimals: u32) -> f64;
}

impl RoundTo for f64 {
    fn round_to(self, decimals: u32) -> f64 {
        let factor = 10f64.powi(decimals as i32);
        (self * factor).round() / factor
    }
}

struct ConnHandle {
    stream: TcpStream,
    dropped_by_pause: Arc<AtomicBool>,
}

/// Accept device connections on one shared port; each announces itself in a
/// handshake, then gets its own segmenter.
///
/// Honours the global capture pause: while paused, the listener is closed (so
/// phones are refused and back off) and any active stream is dropped — a pause
/// stops phone recording just as it stops the USB mic.
pub fn serve(root: &Path, port: u16, config: &CaptureConfig) -> ! {
    let conns: Arc<Mutex<HashMap<u64, ConnHandle>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut listener: Option<TcpListener> = None;
    let mut next_id: u64 = 0;
    loop {
        if pause::is_paused(root, Utc::now()) {
            if listener.take().is_some() {
                for (_, conn) in conns.lock().expect("conns lock").drain() {
                    conn.dropped_by_pause.store(true, Ordering::Relaxed);
                    let _ = conn.stream.shutdown(std::net::Shutdown::Both);
                }
                tracing::info!("ingest: paused — not accepting");
            }
            std::thread::sleep(PAUSE_POLL);
            continue;
        }
        if listener.is_none() {
            match TcpListener::bind(("0.0.0.0", port)) {
                Ok(bound) => {
                    bound.set_nonblocking(true).expect("nonblocking listener");
                    tracing::info!(port, "ingest: listening");
                    listener = Some(bound);
                }
                Err(err) => {
                    tracing::error!(port, error = %err, "ingest: cannot bind — retrying");
                    std::thread::sleep(PAUSE_POLL);
                    continue;
                }
            }
        }
        match listener.as_ref().expect("bound above").accept() {
            Ok((stream, _addr)) => {
                // Reads on the connection are bounded (READ_TIMEOUT), so
                // blocking mode is what the pump wants.
                let _ = stream.set_nonblocking(false);
                let id = next_id;
                next_id += 1;
                let dropped = Arc::new(AtomicBool::new(false));
                if let Ok(clone) = stream.try_clone() {
                    conns.lock().expect("conns lock").insert(
                        id,
                        ConnHandle {
                            stream: clone,
                            dropped_by_pause: Arc::clone(&dropped),
                        },
                    );
                }
                let root: PathBuf = root.to_owned();
                let config = config.clone();
                let conns = Arc::clone(&conns);
                std::thread::spawn(move || {
                    handle_connection(&stream, &root, &config, &dropped);
                    conns.lock().expect("conns lock").remove(&id);
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_IDLE);
            }
            Err(err) => {
                tracing::warn!(error = %err, "ingest: accept failed");
                std::thread::sleep(ACCEPT_IDLE);
            }
        }
    }
}
