//! Speech detection: the fleet's measurement of stored segments and the live
//! tier's streaming cut. A level can say a segment is loud; only this can say
//! it is speech.
//!
//! The network is silero, embedded (see `assets/README.md`). It consumes fixed
//! 512-sample windows of 16 kHz mono and carries a state tensor between them,
//! so the caller must not reorder or skip windows.

use crate::decode;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Recorded when the audio could not be decoded at all. Negative seconds are
/// impossible, which is the point: ⚠ "we could not look" must never be stored
/// as the 0.0 that means "nobody spoke", or a sweep deletes audio it never
/// examined. Here so every writer of a speech measurement shares it.
pub const UNKNOWN_SECONDS: f64 = -1.0;

/// What silero was trained on, and what every segment is decoded to.
pub const RATE: u32 = 16_000;
/// The window the 16 kHz model expects. Not a tunable: the graph is shaped for it.
/// Public because a STREAMING caller must cut its reads to exactly this.
pub const WINDOW: usize = 512;
/// ⚠ silero v5+ prepends this many samples of the previous window, so the model
/// is fed `CONTEXT + WINDOW`. The input shape is dynamic, so omitting the
/// context is accepted silently and returns near-zero probability on obvious
/// speech, which looks like a quiet room. Matches silero's own
/// `utils_vad.OnnxWrapper.__call__`.
const CONTEXT: usize = 64;

/// "Are we sure it is speech." Silero's default.
const THRESHOLD: f32 = 0.5;
/// Leaving speech is deliberately harder than entering it, so one weak window
/// mid-word does not split a region in two. Silero's own hysteresis margin.
const EXIT_THRESHOLD: f32 = THRESHOLD - 0.15;
/// Regions shorter than this are noise, not talking.
const MIN_SPEECH_MS: f64 = 250.0;
/// Silence shorter than this is a pause inside speech, not the end of it.
const MIN_SILENCE_MS: f64 = 300.0;

/// Phone mics capture un-gained, ~25-40 dB below the USB mic, which leaves
/// audible speech below the detector's sensitivity. The peak is lifted to this
/// before detection; the ASR still sees the original audio.
///
/// ⚠ No cap on the lift: a cap is a floor below which a microphone reads 0.0 s
/// of speech, and such a segment gets no transcribe job.
const TARGET_PEAK: f32 = 0.5;

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

/// ⚠ One session for the whole process, never dropped. With `load-dynamic`,
/// ONNX Runtime's destructors run after the library is unloaded and the
/// process segfaults at exit. It also means the model (2 s to construct
/// against 0.5 s per detection) is built once per process.
static SESSION: OnceLock<Mutex<ort::session::Session>> = OnceLock::new();

/// A handle to the process-wide detector. Cheap to create; holding one across a
/// batch serialises inference, which is what the single-thread policy wants.
pub struct Detector {
    session: MutexGuard<'static, ort::session::Session>,
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
    TARGET_PEAK / peak
}

/// One real inference on silence, to prove the whole chain works before the
/// scanner trusts it: the dynamic library resolved, the API level matched, the
/// embedded model parsed, and the CPU can execute what the library emits.
///
/// ⚠ It cannot catch SIGILL, which kills the process rather than returning an
/// error; that is excluded by loading a baseline-built runtime (see
/// recalld/Cargo.toml).
///
/// # Errors
/// Whatever prevented the inference, so the caller can stand down rather than
/// report a silent room.
pub fn self_test() -> Result<(), Error> {
    let mut detector = Detector::load()?;
    let quiet = vec![0.0_f32; WINDOW * 2];
    detector.probabilities(&quiet)?;
    Ok(())
}

fn build_session() -> Result<ort::session::Session, Error> {
    ort::session::Session::builder()
        .map_err(|e| Error::Model(e.to_string()))?
        // One thread: onnxruntime defaults to every core, and this runs as a
        // background scanner on a shared 4-core server.
        .with_intra_threads(1)
        .map_err(|e| Error::Model(e.to_string()))?
        .with_inter_threads(1)
        .map_err(|e| Error::Model(e.to_string()))?
        .commit_from_memory(MODEL)
        .map_err(|e| Error::Model(e.to_string()))
}

/// What one window of inference hands to the next: silero's state tensor and
/// the 64 samples of context it prepends. A batch pass starts fresh; a live
/// [`Stream`] keeps one for its whole life.
struct Carried {
    state: Vec<f32>,
    context: Vec<f32>,
}

impl Carried {
    fn new() -> Self {
        Self {
            state: vec![0.0_f32; 2 * 128],
            context: vec![0.0_f32; CONTEXT],
        }
    }
}

impl Detector {
    /// # Errors
    /// If the dynamic ONNX Runtime cannot be loaded (wrong API level, library
    /// absent) or the embedded network fails to parse.
    pub fn load() -> Result<Self, Error> {
        // Built fallibly, not via `get_or_init`, so a missing or mismatched
        // runtime is an error the caller can stand down on, not a panic.
        if SESSION.get().is_none() {
            let _ = SESSION.set(Mutex::new(build_session()?));
        }
        let cell = SESSION
            .get()
            .ok_or_else(|| Error::Model("session unavailable".to_owned()))?;
        // Poisoning means a previous inference panicked; the session itself is
        // still valid, so recover rather than refuse to measure ever again.
        let session = cell
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(Self { session })
    }

    /// Per-window speech probabilities for 16 kHz mono samples. Public because
    /// only this view can pin the model's input contract: hysteresis over
    /// all-zero probabilities looks exactly like a quiet room.
    ///
    /// # Errors
    /// If the network fails.
    pub fn probabilities(&mut self, samples: &[f32]) -> Result<Vec<f32>, Error> {
        let gain = detection_gain(samples.iter().fold(0.0_f32, |m, s| m.max(s.abs())));
        let mut carried = Carried::new();
        let mut out = Vec::with_capacity(samples.len() / WINDOW);
        for chunk in samples.as_chunks::<WINDOW>().0 {
            out.push(self.window(chunk, gain, &mut carried)?);
        }
        Ok(out)
    }

    /// One window, with the caller holding the state that crosses windows.
    /// Both the batch pass above and [`Stream`] go through here, so a live
    /// utterance and a stored segment are measured by the same inference.
    fn window(&mut self, chunk: &[f32], gain: f32, carried: &mut Carried) -> Result<f32, Error> {
        let mut framed = Vec::with_capacity(CONTEXT + WINDOW);
        framed.extend_from_slice(&carried.context);
        framed.extend(chunk.iter().map(|s| s * gain));
        carried.context = framed[framed.len() - CONTEXT..].to_vec();
        let input = ort::value::Tensor::from_array(([1, CONTEXT + WINDOW], framed))
            .map_err(|e| Error::Model(e.to_string()))?;
        let state_tensor = ort::value::Tensor::from_array(([2, 1, 128], carried.state.clone()))
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
        let probability = prob[0];
        let (_, next) = outputs["stateN"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Model(e.to_string()))?;
        carried.state = next.to_vec();
        Ok(probability)
    }

    /// Speech regions in 16 kHz mono samples.
    ///
    /// # Errors
    /// Only if the network itself fails; "no speech" is an empty vec, not an error.
    pub fn regions(&mut self, samples: &[f32]) -> Result<Vec<Region>, Error> {
        Ok(regions_from_probabilities(&self.probabilities(samples)?))
    }

    /// Where the speech is in a stored segment of any container, in seconds
    /// from its start.
    ///
    /// # Errors
    /// `Undecodable` if ffmpeg produced nothing: "we could not look" is never
    /// zero speech.
    pub fn speech_regions(&mut self, path: &Path) -> Result<Vec<Region>, Error> {
        let pcm = decode::decode_s16(path, RATE).ok_or(Error::Undecodable)?;
        if pcm.is_empty() {
            return Err(Error::Undecodable);
        }
        self.regions(&decode::to_f32(&pcm))
    }

    /// Seconds of speech in a stored segment of any container.
    ///
    /// # Errors
    /// As [`Detector::speech_regions`].
    pub fn speech_seconds(&mut self, path: &Path) -> Result<f64, Error> {
        Ok(self.speech_regions(path)?.iter().map(Region::seconds).sum())
    }
}

/// Every speech region in a finished array of probabilities: a whole stored
/// segment, decided at once. Public so tests can pin the policy, which lives in
/// [`Splitter`].
#[must_use]
pub fn regions_from_probabilities(probs: &[f32]) -> Vec<Region> {
    let mut splitter = Splitter::new();
    let mut regions: Vec<Region> = Vec::new();
    for &p in probs {
        regions.extend(splitter.push(p).map(Windows::region));
    }
    regions.extend(splitter.flush().map(Windows::region));
    regions
}

/// A span in windows: a streaming caller slices its buffer by these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Windows {
    pub first: usize,
    /// One past the last window, like any Rust range.
    pub end: usize,
}

impl Windows {
    /// The same span in seconds from the start of the stream.
    #[must_use]
    pub fn region(self) -> Region {
        Region {
            start: self.first as f64 * window_seconds(),
            end: self.end as f64 * window_seconds(),
        }
    }
}

/// How long one [`WINDOW`] is, for a streaming caller converting to seconds.
#[must_use]
pub fn window_seconds() -> f64 {
    f64::from(WINDOW as u32) / f64::from(RATE)
}

fn min_silence_windows() -> usize {
    (MIN_SILENCE_MS / 1000.0 / window_seconds()).ceil() as usize
}

/// A closed span, or `None` if it was too short to be talking. The one place
/// `MIN_SPEECH_MS` is applied, shared by the offline and streaming paths.
fn region_if_long_enough(begin: usize, end: usize) -> Option<Windows> {
    let span = Windows { first: begin, end };
    (span.region().seconds() * 1000.0 >= MIN_SPEECH_MS).then_some(span)
}

/// The region policy itself, decided one window at a time.
///
/// The only implementation: [`regions_from_probabilities`] is a fold over it,
/// so the live tier and the archive cannot disagree about where an utterance
/// ended.
#[derive(Debug, Default)]
pub struct Splitter {
    index: usize,
    start: Option<usize>,
    quiet_run: usize,
}

impl Splitter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one window's probability. `Some` means a region just closed and the
    /// caller may cut those windows out of its buffer.
    pub fn push(&mut self, probability: f32) -> Option<Windows> {
        let i = self.index;
        self.index += 1;
        if probability >= THRESHOLD {
            if self.start.is_none() {
                self.start = Some(i);
            }
            self.quiet_run = 0;
            return None;
        }
        let begin = self.start?;
        if probability < EXIT_THRESHOLD {
            self.quiet_run += 1;
        }
        if self.quiet_run < min_silence_windows() {
            return None;
        }
        self.start = None;
        let end = i + 1 - self.quiet_run;
        self.quiet_run = 0;
        region_if_long_enough(begin, end)
    }

    /// Close whatever is open because the stream ended. A live agent calls this
    /// on shutdown so the last sentence is not lost to the exit.
    pub fn flush(&mut self) -> Option<Windows> {
        let begin = self.start.take()?;
        self.quiet_run = 0;
        region_if_long_enough(begin, self.index)
    }

    /// Windows fed so far: the caller's clock for what it has buffered.
    #[must_use]
    pub const fn windows_seen(&self) -> usize {
        self.index
    }

    /// The window an open region started at, if one is open. A live agent needs
    /// it to bound how long it will wait before cutting a sentence itself.
    #[must_use]
    pub const fn open_since(&self) -> Option<usize> {
        self.start
    }

    /// Cut an open region at the current window, whatever the probabilities say:
    /// for a speaker who has not paused long enough to end a region, in a tier
    /// that promises latency.
    pub fn cut(&mut self) -> Option<Windows> {
        let begin = self.start?;
        self.start = Some(self.index);
        self.quiet_run = 0;
        region_if_long_enough(begin, self.index)
    }
}

/// A live detector: one window at a time, carrying state across the whole
/// stream.
///
/// ⚠ The gain is fixed for the life of the stream. [`Detector::probabilities`]
/// derives it from the buffer's own peak, which is right for a stored segment
/// and wrong for a stream: normalising each 32 ms window to its own peak makes
/// room tone as loud as a voice. The USB mic needs no gain: pass 1.0.
pub struct Stream {
    detector: Detector,
    carried: Carried,
    gain: f32,
}

impl Stream {
    /// # Errors
    /// If the ONNX runtime or the embedded network cannot be loaded.
    pub fn open(gain: f32) -> Result<Self, Error> {
        Ok(Self {
            detector: Detector::load()?,
            carried: Carried::new(),
            gain,
        })
    }

    /// The speech probability of exactly [`WINDOW`] samples.
    ///
    /// # Errors
    /// If the network fails, or the window is the wrong length (a dynamic input
    /// shape would otherwise answer it with a plausible number).
    pub fn probability(&mut self, window: &[f32]) -> Result<f32, Error> {
        if window.len() != WINDOW {
            return Err(Error::Model(format!(
                "a window is {WINDOW} samples, not {}",
                window.len()
            )));
        }
        self.detector.window(window, self.gain, &mut self.carried)
    }
}

/// The vendored silero network, embedded so no deployment step can forget it
/// and no runtime path can drift.
pub const MODEL: &[u8] = include_bytes!("../assets/silero_vad_16k_op15.onnx");
