//! The local-mic capture pipeline: a PCM producer (sox on `CoreAudio`, or
//! ffmpeg on ALSA) piped into the ffmpeg segmenter through a metered pump,
//! watched by the dead-segment watchdog.
//!
//! The split keeps capture gap-free: sox does not drop samples, and ffmpeg only
//! sees a clean continuous stream. sox's one known failure, a `CoreAudio` read
//! that wedges to digital zeros while the device stays healthy, is covered by
//! the watchdog: it cycles the producer when closed segments decode to pure
//! silence (or rotation stalls), so a wedge costs minutes.

use crate::events;
use crate::meter::{SILENCE_PEAK, StreamMeter};
use crate::pause;
use crate::segmenter::{CaptureConfig, build_segment_argv, segment_output_pattern};
use audiocore::decode::decode_native_s16;
use audiocore::names::{parse_segment_start, segment_glob};
use chrono::{DateTime, Utc};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How often the watchdog looks, and how many consecutive digital-silence
/// segments mean the producer's device read has wedged. Two (not one) because
/// a single segment could straddle the moment a wedge began.
const WATCH_POLL: Duration = Duration::from_secs(30);
const DEAD_SEGMENTS_TO_CYCLE: u32 = 2;
/// producer -> segmenter pump chunk (matches the ingest pump's socket chunk).
const PUMP_CHUNK_BYTES: usize = 65536;
/// Grace for ffmpeg to finalise the current segment on a pause before
/// force-killing, so a pause never leaves a truncated segment.
const TERM_GRACE: Duration = Duration::from_secs(10);
/// How often the pipe re-checks the pause while running / while parked.
const STOP_POLL: Duration = Duration::from_secs(1);

/// Which program opens the audio device. `Sox` is the Mac's path (`CoreAudio`,
/// sample-perfect); `Alsa` is ffmpeg reading ALSA on the Linux recorder,
/// rather than teaching sox a second platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Producer {
    Sox,
    Alsa,
}

/// ffmpeg argv for an ALSA device: s16le on stdout, downmixed and resampled
/// to the segmenter's shape.
pub fn alsa_argv(
    device: Option<&str>,
    sample_rate: u32,
    channels: u16,
    max_seconds: Option<u64>,
) -> Vec<String> {
    let mut argv: Vec<String> = [
        "ffmpeg",
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "alsa",
    ]
    .map(String::from)
    .to_vec();
    // ⚠ `-channels` belongs before `-i`; `-ac` after it is not the same thing.
    // Before `-i` configures the input. Without it the ALSA demuxer opens the
    // device at two channels, and a mono-only microphone refuses:
    //
    //     [in#0] cannot set channel count to 2 (Invalid argument)
    argv.splice(argv.len().., ["-channels".to_owned(), channels.to_string()]);
    argv.push("-i".to_owned());
    argv.push(device.unwrap_or("default").to_owned());
    if let Some(seconds) = max_seconds {
        argv.extend(["-t".into(), seconds.to_string()]);
    }
    argv.extend(
        [
            "-ac",
            &channels.to_string(),
            "-ar",
            &sample_rate.to_string(),
            "-f",
            "s16le",
            "-",
        ]
        .map(String::from),
    );
    argv
}

/// sox argv for the pinned `CoreAudio` device. An unknown device name makes sox
/// fail hard (the launchd agent crash-loops visibly), never a silent fallback
/// to the system default, which a Bluetooth handsfree mic can grab.
pub fn sox_argv(
    device: Option<&str>,
    sample_rate: u32,
    channels: u16,
    max_seconds: Option<u64>,
) -> Vec<String> {
    let mut argv: Vec<String> = ["sox", "-q"].map(String::from).to_vec();
    match device {
        Some(name) => argv.extend(["-t".into(), "coreaudio".into(), name.into()]),
        None => argv.push("-d".into()),
    }
    argv.extend(
        [
            "-c",
            &channels.to_string(),
            "-r",
            &sample_rate.to_string(),
            "-b",
            "16",
            "-t",
            "raw",
            "-e",
            "signed-integer",
            "-",
        ]
        .map(String::from),
    );
    if let Some(seconds) = max_seconds {
        argv.extend(["trim".into(), "0".into(), seconds.to_string()]);
    }
    argv
}

/// True when the segment holds nothing or decodes to pure digital zeros, the
/// signature of a wedged device read (a live room's noise floor is never
/// zero). Unreadable is not a verdict: never cycle on doubt.
pub fn segment_is_digital_silence(path: &Path) -> bool {
    match path.metadata() {
        Ok(meta) if meta.len() == 0 => return true,
        Ok(_) => {}
        Err(_) => return false,
    }
    let Some(pcm) = decode_native_s16(path) else {
        return false;
    };
    if pcm.is_empty() {
        return true;
    }
    pcm.as_chunks::<2>()
        .0
        .iter()
        .all(|pair| i32::from(i16::from_le_bytes(*pair)).abs() < SILENCE_PEAK)
}

/// Floor the cutoff to the second: segment names carry whole seconds, so the
/// run's own first segment would otherwise miss the bar by microseconds and
/// liveness would wait a full extra segment.
fn starts_after(name: &str, cutoff: DateTime<Utc>) -> bool {
    parse_segment_start(name).is_some_and(|start| {
        start >= cutoff - chrono::Duration::nanoseconds(i64::from(cutoff.timestamp_subsec_nanos()))
    })
}

/// Why a `record` run ended; the caller's respawn/park decision hangs on it.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Ended {
    /// The pause file stopped it; park and resume later.
    Paused,
    /// Producer/segmenter ended on their own (device gone, wedge cycled, EOF).
    ProducerEnded,
}

fn mark_alive(out_dir: &Path) {
    let _ = std::fs::write(out_dir.join(".alive"), b"");
}

/// The watchdog loop body, one poll: returns `Some(reason)` when the producer
/// must be cycled. Split from the thread so the decision is testable.
struct Watchdog {
    out_dir: PathBuf,
    source_id: String,
    stall_after: Duration,
    started_utc: DateTime<Utc>,
    dead_streak: u32,
    last_checked: Option<String>,
    closed_live: bool,
    newest_seen: Option<String>,
    newest_for: Duration,
}

impl Watchdog {
    fn poll(&mut self, elapsed: Duration) -> Option<String> {
        let names: Vec<String> = segment_glob(&self.out_dir, &self.source_id)
            .into_iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(String::from))
            .collect();
        let newest = names.last()?;
        if Some(newest) == self.newest_seen.as_ref() {
            self.newest_for += elapsed;
        } else {
            self.newest_seen = Some(newest.clone());
            self.newest_for = Duration::ZERO;
        }
        let stalled = self.newest_for >= self.stall_after;
        // The last name is the open segment; before it is the newest closed one.
        if names.len() > 1 {
            let closed = &names[names.len() - 2];
            if Some(closed) != self.last_checked.as_ref() {
                self.last_checked = Some(closed.clone());
                if segment_is_digital_silence(&self.out_dir.join(closed)) {
                    self.dead_streak += 1;
                    self.closed_live = false;
                } else {
                    self.dead_streak = 0;
                    self.closed_live = starts_after(closed, self.started_utc);
                }
            }
        }
        if self.dead_streak >= DEAD_SEGMENTS_TO_CYCLE || stalled {
            return Some(if stalled {
                "stalled producer".into()
            } else {
                format!("{} silent segments", self.dead_streak)
            });
        }
        if self.closed_live && !stalled {
            mark_alive(&self.out_dir);
        }
        None
    }
}

/// Capture `source_id` into `root/<source_id>/` as rotating segment files
/// until the pause fires or the producer ends. The fan-out live tap rides the
/// segmenter (`fanout` in `build_segment_argv`), so recall-live never opens
/// the device.
#[allow(
    clippy::too_many_lines,
    reason = "the capture loop in one piece (doc above)"
)]
pub fn record(
    root: &Path,
    source_id: &str,
    device: Option<&str>,
    producer_kind: Producer,
    config: &CaptureConfig,
    max_seconds: Option<u64>,
) -> Ended {
    let out_dir = root.join(source_id);
    if let Err(err) = std::fs::create_dir_all(&out_dir) {
        tracing::error!(source = source_id, error = %err, "capture: cannot create source dir");
        return Ended::ProducerEnded;
    }
    let pattern = segment_output_pattern(root, source_id, config.codec.container_ext());
    let started_utc = Utc::now();
    let producer_argv = match producer_kind {
        Producer::Sox => sox_argv(device, config.sample_rate, config.channels, max_seconds),
        Producer::Alsa => alsa_argv(device, config.sample_rate, config.channels, max_seconds),
    };
    let mut producer = match Command::new(&producer_argv[0])
        .args(&producer_argv[1..])
        .env("TZ", "UTC")
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            tracing::error!(error = %err, "capture: cannot start sox");
            return Ended::ProducerEnded;
        }
    };
    let mut consumer = match Command::new(&config.program)
        .args(build_segment_argv(config, &pattern, true))
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            tracing::error!(error = %err, "capture: cannot start segmenter");
            let _ = producer.kill();
            return Ended::ProducerEnded;
        }
    };
    let producer_out = producer.stdout.take().expect("piped stdout");
    let consumer_in = consumer.stdin.take().expect("piped stdin");
    let pump_dead = Arc::new(AtomicBool::new(false));
    let pump = std::thread::spawn({
        let out_dir = out_dir.clone();
        let mut meter = StreamMeter::new(config.sample_rate, config.channels);
        let pump_dead = Arc::clone(&pump_dead);
        move || {
            // The archive write comes first; a chunk whose peak clears the
            // silence floor refreshes the liveness marker.
            let mut reader = producer_out;
            let mut writer = consumer_in;
            let mut buf = vec![0u8; PUMP_CHUNK_BYTES];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break, // producer EOF -> segmenter finalises
                    Ok(n) => n,
                };
                if writer.write_all(&buf[..n]).is_err() {
                    // The segmenter died mid-run: tell record() so it can
                    // terminate sox, since a producer against a full pipe wedges.
                    pump_dead.store(true, Ordering::Relaxed);
                    break;
                }
                if meter.feed(&buf[..n]) >= SILENCE_PEAK {
                    mark_alive(&out_dir);
                }
            }
            // Close-flush may hit the same dead pipe the write just did: drop
            // does it, errors and all.
        }
    });
    tracing::info!(
        source = source_id,
        device = device.unwrap_or("(default)"),
        "capture: listening"
    );

    let mut watchdog = Watchdog {
        out_dir: out_dir.clone(),
        source_id: source_id.into(),
        // Rotation normally happens every segment; three lengths of nothing
        // means the producer is delivering no samples at all.
        stall_after: Duration::from_secs(u64::from(3 * config.segment_seconds).max(90)),
        started_utc,
        dead_streak: 0,
        last_checked: None,
        closed_live: false,
        newest_seen: None,
        newest_for: Duration::ZERO,
    };
    let mut last_watch = Instant::now();
    let ended;
    loop {
        match consumer.try_wait() {
            Ok(Some(_)) | Err(_) => {
                ended = Ended::ProducerEnded;
                break;
            }
            Ok(None) => {}
        }
        if pause::is_paused(root, Utc::now()) {
            // Close the producer first, then let the segmenter finalise the
            // current segment on the resulting EOF, so no audio is lost.
            let _ = producer.kill();
            let _ = producer.wait();
            wait_grace(&mut consumer, TERM_GRACE);
            ended = Ended::Paused;
            break;
        }
        if pump_dead.load(Ordering::Relaxed) {
            let _ = producer.kill();
            let _ = producer.wait();
            ended = Ended::ProducerEnded;
            break;
        }
        if last_watch.elapsed() >= WATCH_POLL {
            let elapsed = last_watch.elapsed();
            last_watch = Instant::now();
            if let Some(why) = watchdog.poll(elapsed) {
                tracing::warn!(
                    source = source_id,
                    why,
                    "capture: dead stream — cycling the producer"
                );
                events::record(root, events::PRODUCER_CYCLED, source_id, Some(&why));
                let _ = producer.kill();
                let _ = producer.wait();
                wait_grace(&mut consumer, TERM_GRACE);
                ended = Ended::ProducerEnded;
                break;
            }
        }
        std::thread::sleep(STOP_POLL);
    }
    // Safety net: ensure both ends are gone; the pump then sees EOF and exits.
    let _ = producer.kill();
    let _ = producer.wait();
    wait_grace(&mut consumer, TERM_GRACE);
    let _ = pump.join();
    tracing::info!(source = source_id, ?ended, "capture: stopped");
    ended
}

/// Wait for the segmenter to finalise; force-kill only if it overruns.
fn wait_grace(child: &mut Child, grace: Duration) {
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// How often a store-and-forward recorder says it is alive.
///
/// ⚠ The heartbeat is not the delivery: a recorder delivers nothing both when
/// paused and when its microphone is dead, and liveness derived from arriving
/// segments cannot tell those apart. Sixty seconds, the segment length.
const BEAT_EVERY: Duration = Duration::from_mins(1);

/// What this recorder tells the fleet about itself.
///
/// `streaming` is false by construction: on the store-and-forward path audio
/// reaches the fleet by upload. `mic_ok` is what the last producer start
/// actually did.
pub fn beat_body(source_id: &str, mic_ok: bool) -> serde_json::Value {
    serde_json::json!({
        "device": source_id,
        "app": "linux",
        "version": env!("CARGO_PKG_VERSION"),
        "streaming": false,
        "micOk": mic_ok,
    })
}

/// Beat until the process ends, on its own thread so a fleet that stops
/// answering slows nothing down.
fn spawn_beat(source_id: &str, url: &str, mic_ok: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    let source_id = source_id.to_owned();
    let url = url.to_owned();
    std::thread::spawn(move || {
        loop {
            let ok = mic_ok.load(std::sync::atomic::Ordering::Relaxed);
            crate::beat_relay::forward(&beat_body(&source_id, ok), &url);
            std::thread::sleep(BEAT_EVERY);
        }
    });
}

/// Park while paused, run while active, re-park when a pause interrupts;
/// exit (for the `KeepAlive` respawn) only when a run ends for a non-pause
/// reason. Marks RESUME/PAUSE transitions in the capture log, best-effort.
pub fn serve_paused_aware(
    root: &Path,
    source_id: &str,
    device: Option<&str>,
    producer_kind: Producer,
    config: &CaptureConfig,
    max_seconds: Option<u64>,
    beat_url: Option<&str>,
) -> ! {
    events::register(
        root,
        source_id,
        match producer_kind {
            Producer::Sox => "coreaudio",
            Producer::Alsa => "alsa",
        },
    );
    // Starts true: nothing has failed yet, and `micOk: false` before the first
    // attempt would cry wolf on every restart.
    let mic_ok = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    if let Some(url) = beat_url {
        spawn_beat(source_id, url, mic_ok.clone());
    }
    loop {
        while pause::is_paused(root, Utc::now()) {
            std::thread::sleep(STOP_POLL);
        }
        events::record(root, events::RESUME, source_id, None);
        let ended = record(root, source_id, device, producer_kind, config, max_seconds);
        // `Ended::Paused` is the only clean end. Anything else means the
        // producer stopped on its own, which is what the beat exists to carry.
        mic_ok.store(ended == Ended::Paused, std::sync::atomic::Ordering::Relaxed);
        if ended == Ended::Paused {
            events::record(root, events::PAUSE, source_id, None);
            continue;
        }
        // Non-pause end: exit so launchd respawns us with a fresh device open
        // (which is what clears a CoreAudio wedge).
        std::process::exit(0);
    }
}
