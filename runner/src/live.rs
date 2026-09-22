//! The instant feed: read the tap, cut it at the pauses, transcribe each
//! utterance the moment the speaker stops, push it to the fleet.
//!
//! ⚠ **This tier holds no archive and no database.** A live turn is
//! PROVISIONAL — the archive pass re-derives the same minute properly and
//! `recalld`'s per-mic writer hides this one when it does — so everything here
//! is best-effort by construction: a dropped utterance costs a few seconds of
//! feed and never a word of the record. That is what lets it drop rather than
//! block, at every step.
//!
//! ⚠ **It reads the TAP, never the device.** Only one process may hold a
//! `CoreAudio` input; two clients on one device starve each other (proven
//! 2026-07-15, when capture and live both got silence). `audiod capture`'s
//! segmenter publishes a second, droppable UDP copy at exactly this format, and
//! this subscribes to that.

use audiocore::vad::{self, Splitter, Stream, WINDOW, Windows};
use chrono::{DateTime, TimeDelta, Utc};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

/// Where `audiod`'s segmenter publishes the tap (`segmenter::FANOUT_URL`).
///
/// ⚠ Overridable (`--tap`) for ONE reason: a test that publishes onto the real
/// socket would feed its fixture to the household's own live agent, and a test
/// that reads it would eat the datagrams that agent is waiting for.
pub const TAP: &str = "udp://127.0.0.1:9876";
/// Datagram payload that fits a loopback packet without IP fragmentation,
/// times the window ffmpeg will buffer before it starts dropping.
const TAP_FIFO: usize = 1316 * 64;
/// How long ffmpeg will sit on a silent socket before giving up, in µs.
///
/// ⚠ **This is what stops an ORPHANED reader holding the tap for ever**, and it
/// is the hazard the Python's SIGTERM handler existed to prevent. `Drop` does
/// not run on SIGTERM, which is how launchd stops an agent — so a restart would
/// otherwise leave an ffmpeg sitting on this port, and the replacement cannot
/// bind a port somebody else holds. Bounding the read bounds the orphan.
///
/// 60 s is far longer than any gap a RUNNING capture can produce: the tap is the
/// segmenter's second output, so datagrams arrive every ~41 ms even in a silent
/// room. A timeout here therefore means capture is STOPPED, not that nobody
/// spoke — which is why the reopen it causes is logged quietly.
pub const TAP_IDLE_US: u64 = 60_000_000;

/// The model name every live turn is stored under. ⚠ Not the real model's name:
/// it is the TIER's name, and both stores key their reconciliation on this exact
/// string (`recalld::work::ingest_live`, `recalld::turns::LIVE_RECONCILED`).
pub const LIVE_MODEL: &str = "live";

/// Utterances that may wait for the shim. Small on purpose: if transcription
/// falls behind the microphone, the feed is already late and a deep queue only
/// makes it later. See `Agent::offer`.
///
/// ⚠ Since [`drain`] joins whatever is waiting into ONE call, a queue this deep
/// is not a deep queue of calls — it is at most [`CALL_SECONDS`] of audio.
pub const BACKLOG: usize = 8;

/// The most audio one transcribe call carries, and so the longest one utterance
/// may run before it is cut and sent anyway.
///
/// ⚠ **The cost of a call is the WINDOW, not the audio.** Whisper pads every
/// input to 30 seconds and runs its encoder over all of it, so a 1 s call costs
/// 2.72 s and a 29 s call 3.37 s — 24% more for 29x the audio, and then a whole
/// extra encoder pass appears at 30 s. Fitted, `2.60 s + 0.0584 s per second`.
///
/// So the only thing this number trades is how long the tier WAITS, and the
/// answer is not 29: that maximises throughput and maximises latency, which is
/// the wrong end for a tier whose entire value is immediacy. At 12 s a full call
/// runs at ~0.25x real time — the backlog drains while the speaker is still
/// talking — and worst-case latency is BOUNDED by the window instead of growing
/// for as long as anyone speaks.
pub const CALL_SECONDS: f64 = 12.0;

/// The longest pause bridged when queued utterances are joined into one call.
/// Past it the speaker has stopped rather than drawn breath, and holding the
/// finished sentence back to wait for the next one is latency for nothing.
pub const BRIDGE_SECONDS: f64 = 2.0;

/// ffmpeg reading the tap and writing raw 16 kHz mono PCM to stdout.
///
/// ⚠ `overrun_nonfatal` + the fifo are what make a slow reader LOSE AUDIO
/// instead of killing the process. That is the right trade here and the wrong
/// one for the archive, which is why the archive does not read this socket.
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
    /// See above; the error IS the rejected utterance, so nothing is lost.
    pub fn join(&mut self, next: Self) -> Result<(), Self> {
        // ⚠ Clamped, not rejected. Both stamps are derived backwards from the
        // clock, so a boundary can round to a few milliseconds of overlap, and
        // splitting a call over that would be an arithmetic artefact.
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

/// Samples in a span of silence, saturating: a nonsense span must not be able to
/// allocate the agent to death.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn samples_in(seconds: f64) -> usize {
    (seconds.clamp(0.0, BRIDGE_SECONDS) * f64::from(vad::RATE)) as usize
}

/// The tap, cut into utterances.
///
/// Owns the buffer, the detector's carried state and the region policy. It does
/// NOT own the clock: [`Self::feed`] is given `now`, so the whole cutting rule is
/// testable without a microphone.
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
            // ⚠ Gain 1.0. See `vad::Stream`: deriving a gain per window makes
            // room tone as loud as a voice. The tap is the USB mic, which has
            // carried this tier unamplified for months.
            stream: Stream::open(1.0)?,
            splitter: Splitter::new(),
            buffer: Vec::new(),
            buffer_first: 0,
        })
    }

    /// Feed exactly one window of samples. `Some` is an utterance that just
    /// closed — because the speaker paused, or because they did not and
    /// [`CALL_SECONDS`] ran out.
    ///
    /// ⚠ **The timestamp is derived BACKWARDS from `now`, not forwards from a
    /// start anchor**, and the reason is that the tap is UDP. A dropped datagram
    /// costs samples, so counting samples forwards from an anchor stamps every
    /// later turn progressively EARLIER than it was said, drifting all evening
    /// with nothing to correct it. Measuring back from the moment the region
    /// closed absorbs every gap instead.
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
        // Everything before the open region — or before the next window, if
        // nobody is talking — can never be wanted again.
        let keep = self
            .splitter
            .open_since()
            .unwrap_or_else(|| self.splitter.windows_seen());
        self.drop_before(keep);
        Ok(utterance)
    }

    /// Whatever is still open, because the stream ended. Without this the
    /// sentence somebody was saying as the agent stopped is simply lost.
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

/// Windows counted as a number. Exact for every count this can reach: a
/// `f64` carries 53 bits, and 2^53 windows is nine million years of audio.
#[allow(clippy::cast_precision_loss)]
fn windows_to_f64(windows: usize) -> f64 {
    windows as f64
}

/// Seconds as a duration, saturating rather than panicking: a nonsense span
/// must not be able to take the agent down.
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
    /// Returns how many crossed. ZERO means the tap was silent for
    /// [`TAP_IDLE_US`] — capture is not running — which is an ordinary state
    /// that repeats every minute of a pause, and must not read like a fault.
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
    /// ⚠ **Never orphan the reader.** An ffmpeg left holding the tap socket
    /// outlives the agent and competes with its own replacement for the
    /// datagrams — the UDP shape of the bug that once wedged the `CoreAudio`
    /// device through a live restart.
    ///
    /// ⚠ And `Drop` does NOT run on SIGTERM, which is how launchd stops this
    /// agent. What guarantees an orphan cannot outlive its usefulness is
    /// [`TAP_IDLE_US`], not this.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The transcribe-and-push side, on its own thread.
///
/// ⚠ **One failure must never end this loop.** Calling the emit bare is exactly
/// what killed live for 40 minutes on 2026-09-03 with the process up, `KeepAlive`
/// satisfied and every health check green: the reader kept reading and nothing
/// was ever transcribed again. Log it and take the next utterance.
pub fn drain(utterances: &Receiver<Utterance>, mut emit: impl FnMut(Utterance)) {
    let mut carried = None;
    loop {
        // ⚠ The carried one is already in hand, so it must NOT wait on the
        // channel: it was refused by the batch before it, not by the queue.
        let Some(mut batch) = carried.take().or_else(|| utterances.recv().ok()) else {
            return;
        };
        // ⚠ **Everything already waiting goes in the SAME call.** A call costs
        // its 30-second window whatever it holds ([`CALL_SECONDS`]), so sending
        // the next half-second separately buys a whole extra encoder pass and
        // the feed falls further behind for as long as anyone keeps talking.
        //
        // Nothing is ever waited FOR. An empty queue means the shim is keeping
        // up, and then this is exactly the old one-utterance-per-call behaviour
        // with no latency added; the joining only happens when it is behind,
        // which is the only time it helps.
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

/// Hand an utterance to the transcriber. `false` means the transcriber is GONE
/// and the caller must stop — not because this one utterance was lost, but
/// because every later one would be too.
///
/// ⚠ **Dropping a FULL queue is the correct answer; a DISCONNECTED one is not.**
/// A full queue means the shim is slower than the microphone, and waiting for it
/// would stall the reader, which loses the same audio a window later and the
/// reader's clock with it. A gone transcriber is the silent-forever failure this
/// tier keeps finding: a reader happily reading and nothing ever transcribed.
/// Stopping lets `KeepAlive` do what it is for.
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
/// ⚠ Empty is a normal answer, not a failure: silero heard a voice and the model
/// found no words in it. Nothing is pushed, and nothing is logged as wrong.
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

/// The spelling the rest of the system stores instants in — python's
/// `datetime.isoformat()`: microseconds, a numeric offset, no `Z`. recalld
/// re-spells what it receives, but sending the house spelling means the wire and
/// the rows agree when anyone reads both.
#[must_use]
pub fn recall_instant(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
}
