//! The instant feed: read the tap, cut it at the pauses, transcribe each
//! utterance the moment the speaker stops, push it to the fleet.
//!
//! This tier holds no archive and no database. A live turn is provisional: the
//! archive pass re-derives the same minute and `recalld`'s per-mic writer then
//! hides it. So everything here is best-effort and drops rather than blocks; a
//! dropped utterance costs a few seconds of feed, never a word of the record.
//!
//! ⚠ It reads the tap, never the device: two `CoreAudio` clients on one input
//! starve each other. `audiod capture`'s segmenter publishes a droppable UDP
//! copy in this format.

use audiocore::vad::{self, Splitter, Stream, WINDOW, Windows};
use chrono::{DateTime, TimeDelta, Utc};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

/// Where `audiod`'s segmenter publishes the tap (`segmenter::FANOUT_URL`).
///
/// Overridable (`--tap`) so tests do not publish to, or read from, the socket
/// the running live agent uses.
pub const TAP: &str = "udp://127.0.0.1:9876";
/// Datagram payload that fits a loopback packet without IP fragmentation,
/// times the window ffmpeg will buffer before it starts dropping.
const TAP_FIFO: usize = 1316 * 64;
/// How long ffmpeg will sit on a silent socket before giving up, in µs.
///
/// ⚠ This is what stops an orphaned ffmpeg holding the port for ever: `Drop`
/// does not run on SIGTERM, which is how launchd stops an agent, and the
/// replacement cannot bind a port another process holds.
///
/// A running capture sends a datagram every ~41 ms even in a silent room, so a
/// 60 s timeout means capture is stopped, and the reopen is logged quietly.
pub const TAP_IDLE_US: u64 = 60_000_000;

/// The model name every live turn is stored under. ⚠ The tier's name, not the
/// real model's: recalld matches this exact string to tell live turns apart.
pub const LIVE_MODEL: &str = "live";

/// Utterances that may wait for the shim. Small on purpose: if transcription
/// falls behind, a deep queue only makes the feed later. See [`offer`].
///
/// [`drain`] joins waiting utterances into calls of up to [`CALL_SECONDS`], so
/// the queue costs fewer calls than it holds utterances.
pub const BACKLOG: usize = 8;

/// The most audio one transcribe call carries, and so the longest one utterance
/// may run before it is cut and sent anyway.
///
/// A call costs its window, not its audio: Whisper pads every input to 30 s,
/// so a 1 s call takes 2.72 s and a 29 s call 3.37 s (fitted: 2.60 s + 0.0584 s
/// per second of audio).
///
/// This number therefore trades only latency. At 12 s a full call runs at ~0.25x
/// real time and worst-case latency stays bounded; 29 s would maximise
/// throughput at the cost of immediacy.
pub const CALL_SECONDS: f64 = 12.0;

/// The longest pause bridged when queued utterances are joined into one call.
/// Past it the speaker has stopped rather than drawn breath.
pub const BRIDGE_SECONDS: f64 = 2.0;

/// ffmpeg reading the tap and writing raw 16 kHz mono PCM to stdout.
///
/// `overrun_nonfatal` and the fifo make a slow reader lose audio instead of
/// killing the process: right here, wrong for the archive, which does not read
/// this socket.
#[must_use]
pub fn tap_argv(tap: &str) -> Vec<String> {
    [
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "s16le",
        "-ar",
        &vad::RATE.to_string(),
        "-ac",
        "1",
        "-i",
        &format!("{tap}?overrun_nonfatal=1&fifo_size={TAP_FIFO}&timeout={TAP_IDLE_US}"),
        "-f",
        "s16le",
        "-",
    ]
    .map(String::from)
    .to_vec()
}

/// One utterance, ready to transcribe: its samples and when it was said.
#[derive(Debug, Clone, PartialEq)]
pub struct Utterance {
    pub samples: Vec<f32>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Utterance {
    /// How much audio this carries.
    #[must_use]
    pub fn seconds(&self) -> f64 {
        (self.end - self.start).as_seconds_f64()
    }

    /// Join `next` onto the end of this one, restoring the pause between them so
    /// the model hears what the room did rather than a splice.
    ///
    /// `Err(next)` means they do not belong in one call — too long a pause, or
    /// the result would outrun [`CALL_SECONDS`] — and hands `next` back to start
    /// the following one.
    ///
    /// # Errors
    /// The rejected utterance itself, so nothing is lost.
    pub fn join(&mut self, next: Self) -> Result<(), Self> {
        // Clamped, not rejected: both stamps are derived back from the clock,
        // so a boundary can round to a few milliseconds of overlap.
        let pause = (next.start - self.end).as_seconds_f64().max(0.0);
        if pause > BRIDGE_SECONDS || (next.end - self.start).as_seconds_f64() > CALL_SECONDS {
            return Err(next);
        }
        self.samples
            .resize(self.samples.len() + samples_in(pause), 0.0);
        self.samples.extend_from_slice(&next.samples);
        self.end = next.end;
        Ok(())
    }
}

/// Samples in a span of silence, capped at [`BRIDGE_SECONDS`] so a nonsense
/// span cannot exhaust memory.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn samples_in(seconds: f64) -> usize {
    (seconds.clamp(0.0, BRIDGE_SECONDS) * f64::from(vad::RATE)) as usize
}

/// The tap, cut into utterances.
///
/// Owns the buffer, the detector's state and the region policy, but not the
/// clock: [`Self::feed`] is given `now`, so the cutting rule is testable.
pub struct Cutter {
    stream: Stream,
    splitter: Splitter,
    /// Samples for windows `[buffer_first, splitter.windows_seen())`.
    buffer: Vec<f32>,
    buffer_first: usize,
}

impl Cutter {
    /// # Errors
    /// If the ONNX runtime or the embedded silero network cannot be loaded.
    pub fn open() -> Result<Self, vad::Error> {
        Ok(Self {
            // Gain 1.0: a per-window gain makes room tone as loud as a voice
            // (see `vad::Stream`).
            stream: Stream::open(1.0)?,
            splitter: Splitter::new(),
            buffer: Vec::new(),
            buffer_first: 0,
        })
    }

    /// Feed exactly one window of samples. `Some` is an utterance that just
    /// closed, because the speaker paused or [`CALL_SECONDS`] ran out.
    ///
    /// ⚠ The timestamp is derived back from `now`, not forward from a start
    /// anchor: the tap is UDP, and counting samples forward would drift earlier
    /// with every dropped datagram.
    ///
    /// # Errors
    /// If the detector fails or the window is not [`WINDOW`] samples.
    pub fn feed(
        &mut self,
        window: &[f32],
        now: DateTime<Utc>,
    ) -> Result<Option<Utterance>, vad::Error> {
        let probability = self.stream.probability(window)?;
        self.buffer.extend_from_slice(window);
        let closed = self.splitter.push(probability).or_else(|| self.overdue());
        let utterance = closed.map(|span| self.take(span, now));
        // Everything before the open region (or the next window, if nobody is
        // talking) is no longer needed.
        let keep = self
            .splitter
            .open_since()
            .unwrap_or_else(|| self.splitter.windows_seen());
        self.drop_before(keep);
        Ok(utterance)
    }

    /// Whatever is still open when the stream ends, so the last sentence is not
    /// lost.
    #[must_use]
    pub fn flush(&mut self, now: DateTime<Utc>) -> Option<Utterance> {
        let span = self.splitter.flush()?;
        Some(self.take(span, now))
    }

    fn overdue(&mut self) -> Option<Windows> {
        let open = self.splitter.open_since()?;
        let windows = self.splitter.windows_seen().saturating_sub(open);
        (seconds_of(windows) >= CALL_SECONDS)
            .then(|| self.splitter.cut())
            .flatten()
    }

    fn take(&mut self, span: Windows, now: DateTime<Utc>) -> Utterance {
        let from = (span.first - self.buffer_first) * WINDOW;
        let to = ((span.end - self.buffer_first) * WINDOW).min(self.buffer.len());
        let samples = self.buffer[from..to].to_vec();
        let behind = seconds_of(self.splitter.windows_seen() - span.first);
        let start = now - delta(behind);
        Utterance {
            end: start + delta(span.region().seconds()),
            start,
            samples,
        }
    }

    fn drop_before(&mut self, window: usize) {
        let drop = (window - self.buffer_first) * WINDOW;
        if drop == 0 {
            return;
        }
        self.buffer.drain(..drop.min(self.buffer.len()));
        self.buffer_first = window;
    }
}

fn seconds_of(windows: usize) -> f64 {
    windows_to_f64(windows) * vad::window_seconds()
}

/// Windows as a number. Exact in practice: 2^53 windows is millions of years.
#[allow(clippy::cast_precision_loss)]
fn windows_to_f64(windows: usize) -> f64 {
    windows as f64
}

/// Seconds as a duration; a negative or unrepresentable span becomes zero
/// rather than a panic.
fn delta(seconds: f64) -> TimeDelta {
    std::time::Duration::try_from_secs_f64(seconds.max(0.0))
        .ok()
        .and_then(|d| TimeDelta::from_std(d).ok())
        .unwrap_or_default()
}

/// ffmpeg on the tap, restartable.
pub struct Tap {
    child: Child,
}

impl Tap {
    /// # Errors
    /// If ffmpeg cannot be started.
    pub fn open(tap: &str) -> std::io::Result<Self> {
        let child = Command::new("ffmpeg")
            .args(tap_argv(tap))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        Ok(Self { child })
    }

    /// Read windows until the tap ends, handing each to `on_window`.
    ///
    /// Returns how many were read. Zero means the tap was silent for
    /// [`TAP_IDLE_US`] because capture is not running: an ordinary state, not a
    /// fault.
    pub fn windows(&mut self, mut on_window: impl FnMut(&[f32]) -> bool) -> usize {
        let Some(stdout) = self.child.stdout.as_mut() else {
            return 0;
        };
        let mut raw = vec![0_u8; WINDOW * 2];
        let mut samples = vec![0.0_f32; WINDOW];
        let mut read = 0;
        loop {
            if stdout.read_exact(&mut raw).is_err() {
                return read; // short read: ffmpeg exited or the pipe was torn down
            }
            read += 1;
            for (sample, bytes) in samples.iter_mut().zip(raw.as_chunks::<2>().0) {
                *sample = f32::from(i16::from_le_bytes(*bytes)) / 32_768.0;
            }
            if !on_window(&samples) {
                return read;
            }
        }
    }
}

impl Drop for Tap {
    /// Kill the reader so an orphaned ffmpeg does not compete with the agent's
    /// replacement for the datagrams. `Drop` does not run on SIGTERM; there,
    /// [`TAP_IDLE_US`] bounds the orphan.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The transcribe-and-push side, on its own thread.
///
/// ⚠ One failure must never end this loop: the reader would keep reading with
/// nothing transcribed and every health check green. `emit` logs its own
/// failures and the loop takes the next utterance.
pub fn drain(utterances: &Receiver<Utterance>, mut emit: impl FnMut(Utterance)) {
    let mut carried = None;
    loop {
        // An utterance carried from the previous batch goes first, without
        // waiting on the channel.
        let Some(mut batch) = carried.take().or_else(|| utterances.recv().ok()) else {
            return;
        };
        // Everything already waiting joins the same call, since a call costs
        // its whole window (see [`CALL_SECONDS`]). Nothing is waited for: when
        // the shim keeps up, each call carries one utterance.
        while let Ok(next) = utterances.try_recv() {
            if let Err(refused) = batch.join(next) {
                carried = Some(refused);
                break;
            }
        }
        let at = batch.start;
        let seconds = batch.seconds();
        emit(batch);
        tracing::debug!(%at, seconds, "utterance handled");
    }
}

/// Hand an utterance to the transcriber. `false` means the transcriber is gone
/// and the caller must stop, so `KeepAlive` restarts the agent.
///
/// A full queue drops the utterance: waiting would stall the reader, which
/// would lose the same audio a window later and skew its clock.
pub fn offer(to: &SyncSender<Utterance>, utterance: Utterance) -> bool {
    match to.try_send(utterance) {
        Ok(()) => true,
        Err(TrySendError::Full(dropped)) => {
            tracing::warn!(at = %dropped.start, "transcription is behind; dropping a live utterance");
            true
        }
        Err(TrySendError::Disconnected(_)) => {
            tracing::error!("the transcriber is gone; the instant feed is off");
            false
        }
    }
}

/// A channel sized for [`BACKLOG`].
#[must_use]
pub fn channel() -> (SyncSender<Utterance>, Receiver<Utterance>) {
    sync_channel(BACKLOG)
}

/// The shim's reply, flattened to the one line a live turn is.
///
/// `None` when there are no words, which is normal: silero heard a voice and
/// the model found nothing in it.
#[must_use]
pub fn spoken(result: &serde_json::Value) -> Option<(String, Option<String>)> {
    let text = result
        .get("segments")?
        .as_array()?
        .iter()
        .filter_map(|s| s.get("text")?.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let text = text.trim().to_owned();
    if text.is_empty() {
        return None;
    }
    let language = result
        .get("language")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    Some((text, language))
}
