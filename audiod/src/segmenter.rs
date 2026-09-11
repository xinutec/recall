//! The ffmpeg segmenter child: reads raw s16le PCM on stdin, writes the ring
//! of UTC-named segment files. Port of `capture.build_segment_argv` +
//! `capture.CaptureConfig` (ingest shape: no fanout tap).
//!
//! ffmpeg stays the encoder for now, deliberately: it keeps this port's output
//! byte-comparable with the Python server's during the shadow period. Native
//! Opus encoding arrives with the fusion engine, which needs the PCM in
//! process anyway (docs/audio-plane.md).

use std::path::Path;

/// The closed set of codecs a segment ring may be written in. An enum rather
/// than the codec string, so a typo'd codec is a compile error and the
/// container extension can never disagree with the codec that filled it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Libopus,
    Flac,
    PcmS16le,
    PcmS24le,
    Aac,
}

impl Codec {
    /// The name ffmpeg's `-c:a` knows this codec by.
    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            Codec::Libopus => "libopus",
            Codec::Flac => "flac",
            Codec::PcmS16le => "pcm_s16le",
            Codec::PcmS24le => "pcm_s24le",
            Codec::Aac => "aac",
        }
    }

    /// The bitrate to ask ffmpeg for, or `None` when the codec is lossless and
    /// a bitrate would be meaningless.
    ///
    /// ⚠ The codec OWNS this, rather than the caller pairing the two, because
    /// the pair can be wrong in a way nothing downstream can see: `-b:a 32k`
    /// alongside `-c:a flac` is accepted and ignored by ffmpeg, so a lossless
    /// capture configured by hand would look configured and be fine, while the
    /// reverse — a lossy codec whose bitrate went missing — would quietly
    /// re-encode the household at a default nobody chose.
    pub fn default_bitrate(self) -> Option<&'static str> {
        match self {
            Codec::Libopus | Codec::Aac => Some("32k"),
            Codec::Flac | Codec::PcmS16le | Codec::PcmS24le => None,
        }
    }

    /// Container file extension for segment files in this codec.
    pub fn container_ext(self) -> &'static str {
        match self {
            Codec::Libopus => "opus",
            Codec::Flac => "flac",
            Codec::PcmS16le | Codec::PcmS24le => "wav",
            Codec::Aac => "m4a",
        }
    }
}

/// Capture parameters. Defaults to FLAC: lossless, because the archive keeps
/// what the microphones heard.
///
/// ⚠ **The default carries the retention decision, and it used to carry the
/// opposite one.** Opus at 32 kbps is perceptually transparent for speech, and
/// on that reasoning it was the default from the beginning — but it destroys
/// PHASE, so two recorders' streams of one room can never be combined
/// (docs/architecture.md). The cost of that default was invisible and
/// irreversible: measured 2026-09-11, every phone segment in the archive is
/// `.opus` even though the phones stream raw PCM and the fleet had already
/// decided on lossless — because `audiod ingest` took the default and nobody
/// passed a flag. A lossy default is the wrong shape for an archive nobody can
/// re-record; a source that wants Opus now has to say so.
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub segment_seconds: u32,
    pub codec: Codec,
    pub bitrate: Option<String>,
    pub loglevel: String,
    /// The segmenter program — "ffmpeg", overridable so tests can substitute a
    /// stub that records what it was fed without needing a codec.
    pub program: String,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            sample_rate: crate::wire::SAMPLE_RATE,
            channels: 1,
            segment_seconds: 60,
            codec: Codec::Flac,
            bitrate: Codec::Flac.default_bitrate().map(Into::into),
            loglevel: "warning".into(),
            program: "ffmpeg".into(),
        }
    }
}

/// Output pattern `<root>/<source_id>/<source_id>-<strftime>.<ext>` — the
/// archive naming contract everything downstream reads.
pub fn segment_output_pattern(root: &Path, source_id: &str, ext: &str) -> String {
    format!(
        "{}/{source_id}/{source_id}-%Y%m%dT%H%M%S.{ext}",
        root.display()
    )
}

/// The live-feed tap (`recall.sources` FANOUT_*): the segmenter's SECOND
/// output, a best-effort UDP copy at live's format. Fire-and-forget — a full
/// or absent receiver just drops packets, so the tap can never backpressure
/// the archive.
const FANOUT_URL: &str = "udp://127.0.0.1:9876?pkt_size=1316";
const FANOUT_SAMPLE_RATE: u32 = 16_000;

fn fanout_output_argv() -> Vec<String> {
    [
        "-ar",
        &FANOUT_SAMPLE_RATE.to_string(),
        "-ac",
        "1",
        "-f",
        "s16le",
        FANOUT_URL,
    ]
    .map(String::from)
    .to_vec()
}

/// Argv that reads raw s16le PCM from stdin and writes segment files. The
/// caller supplies the PCM stream on the child's stdin; the segmenter never
/// touches a device. `fanout` appends the best-effort UDP live tap as a second
/// output, so recall-live never opens the device.
pub fn build_segment_argv(
    config: &CaptureConfig,
    output_pattern: &str,
    fanout: bool,
) -> Vec<String> {
    let mut argv: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        &config.loglevel,
        "-f",
        "s16le",
        "-ar",
        &config.sample_rate.to_string(),
        "-ac",
        &config.channels.to_string(),
        "-i",
        "-",
        "-c:a",
        config.codec.ffmpeg_name(),
    ]
    .into_iter()
    .map(String::from)
    .collect();
    if let Some(bitrate) = &config.bitrate {
        argv.extend(["-b:a".into(), bitrate.clone()]);
    }
    if config.codec == Codec::Libopus {
        argv.extend(["-application".into(), "voip".into()]); // voice-optimised
    }
    argv.extend(
        [
            "-f",
            "segment",
            "-segment_time",
            &config.segment_seconds.to_string(),
            "-reset_timestamps",
            "1",
            "-strftime",
            "1",
            output_pattern,
        ]
        .into_iter()
        .map(String::from),
    );
    if fanout {
        argv.extend(fanout_output_argv());
    }
    argv
}
