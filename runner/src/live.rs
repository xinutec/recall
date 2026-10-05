//! The instant feed: read the tap, cut it at the pauses, transcribe each
//! utterance when the speaker stops, push it to recalld.
//!
//! A live turn is provisional: when the archive pass writes the same span,
//! recalld hides it. So this tier drops rather than blocks; a dropped utterance
//! costs a few seconds of feed, never a word of the record.
//!
//! It reads `audiod`'s UDP tap, never the device: two `CoreAudio` clients on
//! one input starve each other.

use audiocore::vad::{self, Splitter, Stream, WINDOW, Windows};
use chrono::{DateTime, TimeDelta, Utc};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

/// Where `audiod`'s segmenter publishes the tap (`segmenter::FANOUT_URL`).
/// Overridable (`--tap`) so tests stay off the running agent's socket.
pub const TAP: &str = "udp://127.0.0.1:9876";
/// One loopback datagram (1316 bytes, unfragmented) times the packets ffmpeg
/// buffers before it drops.
const TAP_FIFO: usize = 1316 * 64;
/// How long ffmpeg waits on a silent socket before exiting, in µs.
///
/// This bounds an orphaned ffmpeg: `Drop` does not run on SIGTERM (how launchd
/// stops an agent), and the replacement cannot bind a port the orphan holds.
/// Capture sends a datagram every ~41 ms even in a silent room, so a minute of
/// nothing means capture is stopped.
pub const TAP_IDLE_US: u64 = 60_000_000;

/// The `asr_model` every live turn is stored under: the tier's name, not the
/// model's. recalld matches this exact string (`turn_store::LIVE_MODEL`).
pub const LIVE_MODEL: &str = "live";

/// Utterances that may wait for the shim. Small, because when transcription
/// falls behind a deep queue only makes the feed later; [`drain`] joins the
/// waiting ones into fewer calls.
pub const BACKLOG: usize = 8;

/// The most audio one transcribe call carries, and so the longest an utterance
/// runs before it is cut and sent anyway.
///
/// Whisper pads every input to 30 s, so a call costs its window, not its audio:
/// measured 2.60 s + 0.0584 s per second of audio. This number therefore trades
/// only latency; at 12 s a full call runs at ~0.25x real time.
pub const CALL_SECONDS: f64 = 12.0;

/// The longest pause bridged when joining queued utterances into one call.
/// Longer, the speaker has stopped rather than drawn breath.
pub const BRIDGE_SECONDS: f64 = 2.0;

/// ffmpeg reading the tap and writing raw 16 kHz mono PCM to stdout.
///
/// `overrun_nonfatal` and the fifo make a slow reader lose audio instead of
/// killing ffmpeg.
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

#[derive(Debug, Clone, PartialEq)]
pub struct Utterance {
    pub samples: Vec<f32>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Utterance {
    #[must_use]
    pub fn seconds(&self) -> f64 {
        (self.end - self.start).as_seconds_f64()
    }

    /// Append `next`, with the pause between them as silence so the model hears
    /// the room rather than a splice.
    ///
    /// # Errors
    /// `next`, handed back, when the pause exceeds [`BRIDGE_SECONDS`] or the
    /// result would exceed [`CALL_SECONDS`].
    pub fn join(&mut self, next: Self) -> Result<(), Self> {
        // Both stamps are derived from the clock, so adjacent utterances can
        // overlap by a few milliseconds.
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

/// Samples in a span of silence, capped at [`BRIDGE_SECONDS`].
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to [0, BRIDGE_SECONDS]"
)]
fn samples_in(seconds: f64) -> usize {
    (seconds.clamp(0.0, BRIDGE_SECONDS) * f64::from(vad::RATE)) as usize
}

/// Cuts the tap into utterances. Takes the clock as an argument
/// ([`Self::feed`]'s `now`) so the cutting rule is testable.
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
            // No gain: a per-window gain makes room tone as loud as a voice.
            stream: Stream::open(1.0)?,
            splitter: Splitter::new(),
            buffer: Vec::new(),
            buffer_first: 0,
        })
    }

    /// Feed one window. `Some` is an utterance that just closed, because the
    /// speaker paused or [`CALL_SECONDS`] ran out.
    ///
    /// Its time is counted back from `now`, not forward from the start: the tap
    /// is UDP, and counting forward would drift with every dropped datagram.
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
        // Keep only the open region; with nobody talking, nothing.
        let keep = self
            .splitter
            .open_since()
            .unwrap_or_else(|| self.splitter.windows_seen());
        self.drop_before(keep);
        Ok(utterance)
    }

    /// Whatever is still open when the stream ends.
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

#[expect(
    clippy::cast_precision_loss,
    reason = "exact below 2^53 windows, millions of years"
)]
fn windows_to_f64(windows: usize) -> f64 {
    windows as f64
}

/// Seconds as a duration; negative or unrepresentable becomes zero.
fn delta(seconds: f64) -> TimeDelta {
    std::time::Duration::try_from_secs_f64(seconds.max(0.0))
        .ok()
        .and_then(|d| TimeDelta::from_std(d).ok())
        .unwrap_or_default()
}

/// ffmpeg on the tap.
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

    /// Hand windows to `on_window` until the tap ends or it returns `false`.
    /// Returns how many were read; zero usually means capture is not running.
    pub fn windows(&mut self, mut on_window: impl FnMut(&[f32]) -> bool) -> usize {
        let Some(stdout) = self.child.stdout.as_mut() else {
            return 0;
        };
        let mut raw = vec![0_u8; WINDOW * 2];
        let mut samples = vec![0.0_f32; WINDOW];
        let mut read = 0;
        loop {
            if stdout.read_exact(&mut raw).is_err() {
                return read; // ffmpeg exited
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
    /// Kill ffmpeg so it does not hold the port. On SIGTERM this does not run;
    /// [`TAP_IDLE_US`] bounds the orphan instead.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The transcribe-and-push loop, on its own thread. Runs until the channel
/// closes.
///
/// `emit` handles its own failures: if one ended this loop, the reader would
/// keep reading with nothing transcribed and every health check green.
pub fn drain(utterances: &Receiver<Utterance>, mut emit: impl FnMut(Utterance)) {
    let mut carried = None;
    loop {
        let Some(mut batch) = carried.take().or_else(|| utterances.recv().ok()) else {
            return;
        };
        // Whatever is already waiting joins the call, which costs its whole
        // window anyway. Nothing is waited for.
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

/// Hand an utterance to the transcriber; `false` means it is gone and the
/// caller should exit, so launchd restarts the agent.
///
/// A full queue drops the utterance: blocking would stall the reader, which
/// would lose audio anyway and skew its clock.
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

#[must_use]
pub fn channel() -> (SyncSender<Utterance>, Receiver<Utterance>) {
    sync_channel(BACKLOG)
}

/// The shim's reply as one line of text and its language. `None` when the
/// model found no words, which is common: silero heard a voice, Whisper
/// nothing.
#[must_use]
pub fn spoken(reply: &audiocore::shim::asr::Reply) -> Option<(String, Option<String>)> {
    let text = reply
        .segments
        .iter()
        .map(|s| s.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return None;
    }
    Some((text, reply.language.clone()))
}
