use playback::corpus::Utterance;
use playback::plan::{PartSpec, RATE, Rng, Voices, lay_out, normalise_peak, wav};

fn utterance(voice: &str, i: usize) -> Utterance {
    Utterance {
        audio: format!("{voice}/{i}.flac").into(),
        text: format!("{voice} says {i}"),
        speaker: voice.into(),
        lang: "en".into(),
    }
}

fn spec(seconds: f64) -> PartSpec {
    PartSpec {
        name: "p".into(),
        device: "Speaker".into(),
        seconds,
        voices: Voices::Librispeech(vec![]),
    }
}

/// Utterance `i` lasts `i` seconds of a constant tone.
fn decode(u: &Utterance) -> Option<Vec<i16>> {
    let i: usize = u.text.rsplit(' ').next()?.parse().ok()?;
    Some(vec![1000; i * RATE as usize])
}

#[test]
fn turns_alternate_voices_and_skip_bad_lengths() {
    let mut pools = vec![
        (0..20).map(|i| utterance("a", i)).collect::<Vec<_>>(),
        (0..20).map(|i| utterance("b", i)).collect(),
    ];
    let (part, samples) = lay_out(&spec(60.0), &mut pools, &mut Rng::new(7), decode).unwrap();
    let speakers: Vec<&str> = part.turns.iter().map(|t| t.speaker.as_str()).collect();
    assert_eq!(speakers[..4], ["a", "b", "a", "b"]);
    for t in &part.turns {
        assert!((4.0..=15.0).contains(&t.duration), "{t:?}");
    }
    for pair in part.turns.windows(2) {
        let gap = pair[1].offset - (pair[0].offset + pair[0].duration);
        assert!((0.4..1.5).contains(&gap), "gap {gap}");
    }
    let last = part.turns.last().unwrap();
    assert!((part.seconds - (last.offset + last.duration) - 1.0).abs() < 1e-9);
    assert!((samples.len() as f64 / f64::from(RATE) - part.seconds).abs() < 1e-9);
}

#[test]
fn the_same_seed_lays_out_the_same_part() {
    let run = |seed| {
        let mut pools = vec![(0..30).map(|i| utterance("a", i)).collect::<Vec<_>>()];
        let mut rng = Rng::new(seed);
        rng.shuffle(&mut pools[0]);
        lay_out(&spec(30.0), &mut pools, &mut rng, decode)
            .unwrap()
            .0
            .turns
    };
    assert_eq!(run(3), run(3));
    assert_ne!(run(3), run(4));
}

#[test]
fn a_voice_running_dry_is_an_error() {
    let mut pools = vec![vec![utterance("a", 5)]];
    assert!(lay_out(&spec(60.0), &mut pools, &mut Rng::new(1), decode).is_err());
}

#[test]
fn peak_lands_at_minus_three_dbfs() {
    let mut s = vec![0, 100, -200, 50];
    normalise_peak(&mut s);
    assert_eq!(s.iter().map(|x| i32::from(*x).abs()).max(), Some(23197));
    let mut silent = vec![0; 4];
    normalise_peak(&mut silent);
    assert_eq!(silent, [0; 4]);
}

#[test]
fn wav_header_describes_the_samples() {
    let bytes = wav(&[1, -1, 2]);
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), RATE);
    assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 6);
    assert_eq!(bytes.len(), 50);
}
