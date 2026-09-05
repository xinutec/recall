//! Speech detection at ingest (stage D4).
//!
//! `segment_levels` (D2) can say a segment is LOUD; only this can say it is
//! SPEECH. That distinction is what the rest of the stage needs: liveness that
//! means "someone is talking" rather than "bytes arrived", the quiet review's
//! evidence, room prioritisation, and — the reason D3's calibrated rank is
//! parked — a reference built from real speech instead of whatever was loudest.
//!
//! The network is silero, embedded (see `assets/README.md`). It consumes fixed
//! 512-sample windows of 16 kHz mono and carries a state tensor between them,
//! so the caller must not reorder or skip windows.

use audiocore::decode;
use std::path::Path;

/// What silero was trained on, and what every segment is decoded to.
pub const RATE: u32 = 16_000;
/// The window the 16 kHz model expects. Not a tunable: the graph is shaped for it.
const WINDOW: usize = 512;
/// ⚠ silero v5+ prepends this many samples of the PREVIOUS window, so the model
/// is fed `CONTEXT + WINDOW`. The ONNX input shape is dynamic, so omitting the
/// context is accepted silently and simply returns near-zero probability on
/// obvious speech — it looks like a quiet room, not a bug. Verified against
/// silero's own `utils_vad.OnnxWrapper.__call__`.
const CONTEXT: usize = 64;

/// "Are we sure it is speech." Mirrors `recall.vad.silero_speech_regions`, whose
/// default has survived the whole corpus.
const THRESHOLD: f32 = 0.5;
/// Leaving speech is deliberately harder than entering it, so one weak window
/// mid-word does not split a region in two. Silero's own hysteresis margin.
const EXIT_THRESHOLD: f32 = THRESHOLD - 0.15;
/// Regions shorter than this are noise, not talking.
const MIN_SPEECH_MS: f64 = 250.0;
/// Silence shorter than this is a pause inside speech, not the end of it.
const MIN_SILENCE_MS: f64 = 300.0;

/// Phone mics capture un-gained, ~25-40 dB below the USB mic, which once left
/// their clearly audible speech below the detector's sensitivity — gated to
/// silence and dropped though it transcribed perfectly. `recall.vad` fixed that
/// by lifting the peak before detection, and a port that omits it would
/// disagree with Python exactly on the mics that need it most.
const TARGET_PEAK: f32 = 0.5;
/// The bound that stops near-silent room tone being amplified into a false
/// trigger. Without it the gain fix trades one error for the opposite one.
const MAX_GAIN: f32 = 32.0;

/// A span of detected speech, in seconds from the start of the audio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    pub start: f64,
    pub end: f64,
}

impl Region {
    #[must_use]
    pub fn seconds(&self) -> f64 {
        self.end - self.start
    }
}

/// Loads the network once and reuses it. Loading per call cost the Python
/// pipeline 2 s of model construction for 0.5 s of detection — five hours
/// instead of one across a cleanup pass — so the type exists to make reuse the
/// easy path.
pub struct Detector {
    session: ort::session::Session,
}

/// What can go wrong, kept separate from "no speech found" so a broken detector
/// can never be recorded as a silent segment.
#[derive(Debug)]
pub enum Error {
    Model(String),
    Undecodable,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Model(err) => write!(f, "silero: {err}"),
            Self::Undecodable => write!(f, "segment did not decode"),
        }
    }
}

impl std::error::Error for Error {}

#[must_use]
pub fn detection_gain(peak: f32) -> f32 {
    if peak <= 0.0 || peak >= TARGET_PEAK {
        return 1.0;
    }
    (TARGET_PEAK / peak).min(MAX_GAIN)
}

impl Detector {
    /// # Errors
    /// If the embedded network fails to load, which is a build problem, not a
    /// runtime condition.
    pub fn load() -> Result<Self, Error> {
        let session = ort::session::Session::builder()
            .map_err(|e| Error::Model(e.to_string()))?
            .commit_from_memory(super::vad::MODEL)
            .map_err(|e| Error::Model(e.to_string()))?;
        Ok(Self { session })
    }

    /// Per-window speech probabilities for 16 kHz mono samples.
    /// Per-window speech probabilities for 16 kHz mono samples. Public because
    /// it is the only view that can pin the model's INPUT CONTRACT: the region
    /// rules above it are hysteresis, and hysteresis over all-zero probabilities
    /// looks exactly like a quiet room.
    ///
    /// # Errors
    /// If the network fails.
    pub fn probabilities(&mut self, samples: &[f32]) -> Result<Vec<f32>, Error> {
        let gain = detection_gain(samples.iter().fold(0.0_f32, |m, s| m.max(s.abs())));
        let mut state = vec![0.0_f32; 2 * 128];
        let mut context = vec![0.0_f32; CONTEXT];
        let mut out = Vec::with_capacity(samples.len() / WINDOW);
        for chunk in samples.chunks_exact(WINDOW) {
            let mut framed = Vec::with_capacity(CONTEXT + WINDOW);
            framed.extend_from_slice(&context);
            framed.extend(chunk.iter().map(|s| s * gain));
            context = framed[framed.len() - CONTEXT..].to_vec();
            let input = ort::value::Tensor::from_array(([1, CONTEXT + WINDOW], framed))
                .map_err(|e| Error::Model(e.to_string()))?;
            let state_tensor = ort::value::Tensor::from_array(([2, 1, 128], state.clone()))
                .map_err(|e| Error::Model(e.to_string()))?;
            let sr = ort::value::Tensor::from_array(((), vec![i64::from(RATE)]))
                .map_err(|e| Error::Model(e.to_string()))?;
            let outputs = self
                .session
                .run(ort::inputs!["input" => input, "state" => state_tensor, "sr" => sr])
                .map_err(|e| Error::Model(e.to_string()))?;
            let (_, prob) = outputs["output"]
                .try_extract_tensor::<f32>()
                .map_err(|e| Error::Model(e.to_string()))?;
            out.push(prob[0]);
            let (_, next) = outputs["stateN"]
                .try_extract_tensor::<f32>()
                .map_err(|e| Error::Model(e.to_string()))?;
            state = next.to_vec();
        }
        Ok(out)
    }

    /// Speech regions in 16 kHz mono samples.
    ///
    /// # Errors
    /// Only if the network itself fails; "no speech" is an empty vec, not an error.
    pub fn regions(&mut self, samples: &[f32]) -> Result<Vec<Region>, Error> {
        Ok(regions_from_probabilities(&self.probabilities(samples)?))
    }

    /// Seconds of speech in a stored segment of any container.
    ///
    /// # Errors
    /// `Undecodable` if ffmpeg produced nothing — recorded as such, never as
    /// zero speech, because "we could not look" and "nobody spoke" must not
    /// share a value.
    pub fn speech_seconds(&mut self, path: &Path) -> Result<f64, Error> {
        let pcm = decode::decode_s16(path, RATE).ok_or(Error::Undecodable)?;
        if pcm.is_empty() {
            return Err(Error::Undecodable);
        }
        let samples = decode::to_f32(&pcm);
        Ok(self.regions(&samples)?.iter().map(Region::seconds).sum())
    }
}

/// Hysteresis + duration rules over per-window probabilities. Public because
/// it is the whole speech/not-speech POLICY — thresholds, the minimum region,
/// the pause that does not end one — and policy is what a test must pin.
#[must_use]
pub fn regions_from_probabilities(probs: &[f32]) -> Vec<Region> {
    let window_s = f64::from(WINDOW as u32) / f64::from(RATE);
    let min_silence_windows = (MIN_SILENCE_MS / 1000.0 / window_s).ceil() as usize;
    let mut regions: Vec<Region> = Vec::new();
    let mut start: Option<usize> = None;
    let mut quiet_run = 0usize;
    for (i, &p) in probs.iter().enumerate() {
        if p >= THRESHOLD {
            if start.is_none() {
                start = Some(i);
            }
            quiet_run = 0;
        } else if start.is_some() {
            if p < EXIT_THRESHOLD {
                quiet_run += 1;
            }
            if quiet_run >= min_silence_windows {
                let begin = start.take().unwrap_or(i);
                push_if_long_enough(&mut regions, begin, i + 1 - quiet_run, window_s);
                quiet_run = 0;
            }
        }
    }
    if let Some(begin) = start {
        push_if_long_enough(&mut regions, begin, probs.len(), window_s);
    }
    regions
}

fn push_if_long_enough(regions: &mut Vec<Region>, begin: usize, end: usize, window_s: f64) {
    let region = Region {
        start: begin as f64 * window_s,
        end: end as f64 * window_s,
    };
    if region.seconds() * 1000.0 >= MIN_SPEECH_MS {
        regions.push(region);
    }
}

/// The vendored silero network, embedded so no deployment step can forget it
/// and no runtime path can drift.
pub const MODEL: &[u8] = include_bytes!("../assets/silero_vad_16k_op15.onnx");
