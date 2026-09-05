//! The local-mic capture pipeline: a PCM producer (sox on `CoreAudio`) piped
//! into the ffmpeg segmenter through a metered pump, watched by the
//! dead-segment watchdog. Port of `recall.runner` + the capture entrypoint of
//! `recall.cli` (`_cmd_record` / `_serve_paused_aware`).
//!
//! The split is what makes capture gap-free: sox does not drop samples, and
//! ffmpeg only ever sees a clean continuous stream. sox's one known failure —
//! its `CoreAudio` read rarely wedges to digital zeros while the device stays
//! healthy — is covered by the watchdog: it cycles the producer when closed
//! segments decode to pure silence (or rotation stalls), so a wedge costs
//! minutes instead of the rest of the recording.

use crate::meter::{SILENCE_PEAK, StreamMeter};
use crate::pause;
use crate::segmenter::{CaptureConfig, build_segment_argv, segment_output_pattern};
use crate::store;
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
/// Grace for ffmpeg to flush + finalise the current segment on a pause before
/// force-killing — so a pause never leaves a truncated segment file.
const TERM_GRACE: Duration = Duration::from_secs(10);
/// How often the pipe re-checks the pause while running / while parked.
const STOP_POLL: Duration = Duration::from_secs(1);

/// sox argv for the pinned `CoreAudio` device. An unknown device name makes sox
/// fail hard (the launchd agent crash-loops visibly) — never a silent fallback
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

/// True when the segment holds nothing or decodes to pure digital zeros — the
/// signature of a wedged device read (a live room's noise floor is never
/// zero). Unreadable is NOT a verdict: never cycle on doubt.
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
    pcm.chunks_exact(2)
        .all(|pair| i32::from(i16::from_le_bytes([pair[0], pair[1]])).abs() < SILENCE_PEAK)
}

/// Floor the cutoff to the second: segment names carry whole seconds, so the
/// run's own first segment would otherwise miss the bar by microseconds and
/// liveness would wait a full extra segment.
fn starts_after(name: &str, cutoff: DateTime<Utc>) -> bool {
    parse_segment_start(name).is_some_and(|start| {
        start >= cutoff - chrono::Duration::nanoseconds(i64::from(cutoff.timestamp_subsec_nanos()))
    })
}

/// Why a `record` run ended — the caller's respawn/park decision hangs on it.
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
/// must be cycled. Split from the thread for testability of the decision.
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
        // The last name is the open segment; before it is the newest CLOSED one.
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
#[allow(clippy::too_many_lines)]
pub fn record(
    root: &Path,
    source_id: &str,
    device: Option<&str>,
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
    let mut producer = match Command::new("sox")
        .args(&sox_argv(device, config.sample_rate, config.channels, max_seconds)[1..])
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
                    // terminate sox — a producer against a full pipe wedges.
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
            // current segment on the resulting EOF — no audio lost.
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
                store::add_capture_event(
                    root,
                    store::KIND_PRODUCER_CYCLED,
                    Utc::now(),
                    source_id,
                    Some(&why),
                );
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

/// Park while paused, run while active, re-park when a pause interrupts —
/// exiting (for the `KeepAlive` respawn) only when a run ends for a non-pause
/// reason. Durably marks RESUME/PAUSE transitions, best-effort.
pub fn serve_paused_aware(
    root: &Path,
    source_id: &str,
    device: Option<&str>,
    config: &CaptureConfig,
    max_seconds: Option<u64>,
) -> ! {
    store::register_source_kind(root, source_id, "coreaudio");
    loop {
        while pause::is_paused(root, Utc::now()) {
            std::thread::sleep(STOP_POLL);
        }
        store::add_capture_event(root, store::KIND_RESUME, Utc::now(), source_id, None);
        let ended = record(root, source_id, device, config, max_seconds);
        if ended == Ended::Paused {
            store::add_capture_event(root, store::KIND_PAUSE, Utc::now(), source_id, None);
            continue;
        }
        // Non-pause end: exit so launchd respawns us with a fresh device open
        // (which is what clears a CoreAudio wedge).
        std::process::exit(0);
    }
}
