//! What is played: parts, each one loudspeaker's stretch of turns from a few
//! voices, written as one WAV per part and a plan holding every utterance's
//! offset and text.

use crate::corpus::Utterance;
use serde::{Deserialize, Serialize};

/// The rate parts are written at, the rate the house's microphones record.
pub const RATE: u32 = 48_000;
/// Utterances shorter than this are mostly one clause, too little for Whisper's
/// context; longer than this, one turn would dominate a part.
pub const SHORTEST: f64 = 4.0;
pub const LONGEST: f64 = 15.0;
/// Pause between turns, drawn uniformly, so turns are not on a fixed beat.
pub const GAP: (f64, f64) = (0.4, 1.5);
/// Each utterance is scaled so its peak sits here, below clipping and well
/// under mastered music at the same device volume.
pub const PEAK_DBFS: f64 = -3.0;

/// Which voices a part draws from.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Voices {
    /// `LibriSpeech` readers by id; turns go round them in order.
    Librispeech(Vec<String>),
    /// A FLEURS language; FLEURS names no speakers, so turns are its utterances in a shuffled order.
    Fleurs(String),
    /// Turns alternating between the listed sources, for speech that switches
    /// language from turn to turn as a bilingual household does.
    Mix(Vec<Voices>),
}

/// The voice pools of a [`Voices::Mix`], interleaved so turns alternate
/// between its sources: each source is spread over as many pools as the
/// largest has, so a single FLEURS pool beside two readers is heard on
/// every second turn, not every third.
pub fn interleave<T>(sources: Vec<Vec<Vec<T>>>) -> Vec<Vec<T>> {
    let n = sources.iter().map(Vec::len).max().unwrap_or(0);
    let spread: Vec<Vec<Vec<T>>> = sources
        .into_iter()
        .map(|pools| {
            if pools.len() >= n {
                return pools;
            }
            let mut out: Vec<Vec<T>> = (0..n).map(|_| Vec::new()).collect();
            for (i, item) in pools.into_iter().flatten().enumerate() {
                out[i % n].push(item);
            }
            out
        })
        .collect();
    let mut columns: Vec<std::vec::IntoIter<Vec<T>>> =
        spread.into_iter().map(Vec::into_iter).collect();
    let mut out = Vec::new();
    for _ in 0..n {
        for column in &mut columns {
            if let Some(pool) = column.next() {
                out.push(pool);
            }
        }
    }
    out
}

/// One part as asked for: which loudspeaker, how long, whose voices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartSpec {
    pub name: String,
    /// The `CoreAudio` output device's name.
    pub device: String,
    pub seconds: f64,
    pub voices: Voices,
}

/// One played utterance, at `offset` seconds into its part's WAV.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Turn {
    pub offset: f64,
    pub duration: f64,
    pub speaker: String,
    pub lang: String,
    pub text: String,
    pub audio: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Part {
    pub name: String,
    pub device: String,
    /// The WAV's length, trailing silence included.
    pub seconds: f64,
    pub turns: Vec<Turn>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub seed: u64,
    pub parts: Vec<Part>,
}

/// `SplitMix64`: a fixed seed must rebuild the same plan on any machine, which
/// is all this needs from a generator.
pub struct Rng(u64);

impl Rng {
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[lo, hi)`.
    pub fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * ((self.next_u64() >> 11) as f64 / (1u64 << 53) as f64)
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = (self.next_u64() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }
}

/// Scale `samples` so the peak sits at [`PEAK_DBFS`]; silence stays silence.
pub fn normalise_peak(samples: &mut [i16]) {
    let peak = samples
        .iter()
        .map(|s| i32::from(*s).abs())
        .max()
        .unwrap_or(0);
    if peak == 0 {
        return;
    }
    let target = 10f64.powf(PEAK_DBFS / 20.0) * f64::from(i16::MAX);
    let gain = target / f64::from(peak);
    for s in samples.iter_mut() {
        *s = (f64::from(*s) * gain)
            .round()
            .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
    }
}

/// Lay one part out: turns round the pools in order, each drawing its next
/// utterance of acceptable length, until the part reaches its length.
/// `decode` returns an utterance's samples at [`RATE`], `None` if unreadable
/// (skipped). Returns the part and its samples.
pub fn lay_out(
    spec: &PartSpec,
    pools: &mut [Vec<Utterance>],
    rng: &mut Rng,
    mut decode: impl FnMut(&Utterance) -> Option<Vec<i16>>,
) -> Result<(Part, Vec<i16>), String> {
    let rate = f64::from(RATE);
    let mut samples: Vec<i16> = Vec::new();
    let mut turns = Vec::new();
    let mut voice = 0;
    while (samples.len() as f64) / rate < spec.seconds {
        let pool = &mut pools[voice % pools.len()];
        let (utterance, mut audio) = loop {
            let Some(u) = pool.pop() else {
                return Err(format!("{}: a voice ran out of utterances", spec.name));
            };
            let Some(audio) = decode(&u) else { continue };
            let seconds = audio.len() as f64 / rate;
            if (SHORTEST..=LONGEST).contains(&seconds) {
                break (u, audio);
            }
        };
        let gap = rng.uniform(GAP.0, GAP.1);
        samples.extend(std::iter::repeat_n(0, (gap * rate) as usize));
        normalise_peak(&mut audio);
        turns.push(Turn {
            offset: samples.len() as f64 / rate,
            duration: audio.len() as f64 / rate,
            speaker: utterance.speaker,
            lang: utterance.lang,
            text: utterance.text,
            audio: utterance.audio.display().to_string(),
        });
        samples.extend(audio);
        voice += 1;
    }
    samples.extend(std::iter::repeat_n(0, RATE as usize));
    let part = Part {
        name: spec.name.clone(),
        device: spec.device.clone(),
        seconds: samples.len() as f64 / rate,
        turns,
    };
    Ok((part, samples))
}

/// A mono 16-bit WAV of `samples` at [`RATE`].
pub fn wav(samples: &[i16]) -> Vec<u8> {
    let data = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend(b"RIFF");
    out.extend((36 + data).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(RATE.to_le_bytes());
    out.extend((RATE * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(data.to_le_bytes());
    for s in samples {
        out.extend(s.to_le_bytes());
    }
    out
}
