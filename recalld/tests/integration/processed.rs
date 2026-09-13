//! The processed-audio check, against the levels the 2026-09-04 archive actually
//! held. The numbers in these tests are measurements, not fixtures invented to
//! pass: they are the medians `segment_levels` reports for that day.

use recalld::processed::{Levels, MIN_SEGMENTS, gaps, processed_sources, processed_with_evidence};

fn many(source: &str, speech_db: f32, floor_db: f32, n: usize) -> Vec<Levels> {
    (0..n)
        .map(|_| Levels {
            source: source.to_owned(),
            speech_db,
            floor_db,
        })
        .collect()
}

/// The real room on the day geb was unintelligible, at the MEDIAN per-segment
/// gap each source actually shows in `segment_levels` — not a gap between
/// averages, which flatters the real mics and understates geb.
fn sep_fourth() -> Vec<Levels> {
    let mut all = Vec::new();
    all.extend(many("usb", -55.3, -69.6, 40)); // 14.3
    all.extend(many("iphone11", -65.4, -81.3, 40)); // 15.9
    all.extend(many("pixel5", -77.0, -98.7, 40)); // 21.7
    all.extend(many("pixel9", -70.2, -94.9, 40)); // 24.7 — the highest REAL mic
    all.extend(many("geb", -56.6, -127.5, 40)); // 70.9 — the defect
    all
}

#[test]
fn it_names_geb_and_nothing_else() {
    // The whole point: four real microphones and one gated stream, separated by
    // a property of each stream against ITSELF.
    assert_eq!(processed_sources(&sep_fourth()), vec!["geb".to_owned()]);
}

#[test]
fn the_defect_scores_best_on_a_floor_based_ratio() {
    // Why every existing metric missed it. geb's speech-to-floor gap is the
    // LARGEST in the room — read as SNR, it is the best microphone there.
    let measured = gaps(&sep_fourth());
    let best = measured
        .iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .expect("sources");
    assert_eq!(best.0, "geb");
    // And it is not a near thing: 46 dB clear of the next best.
    let others = measured.iter().filter(|(s, _, _)| s != "geb");
    let runner_up = others.fold(f32::MIN, |acc, (_, gap, _)| acc.max(*gap));
    assert!(
        best.1 - runner_up > 45.0,
        "geb {} vs next {runner_up}",
        best.1
    );
}

#[test]
fn a_quiet_house_is_not_a_processed_one() {
    // Silence has no speech level to compare against. Segments below the speech
    // floor are excluded rather than measured, so an empty room cannot be
    // reported as a gated microphone.
    let silent = many("usb", -95.0, -160.0, 40);
    assert!(processed_sources(&silent).is_empty());
    assert!(gaps(&silent).is_empty(), "no usable segment, so no verdict");
}

#[test]
fn too_little_evidence_says_nothing() {
    let barely = many("geb", -56.6, -112.3, MIN_SEGMENTS - 1);
    assert!(processed_sources(&barely).is_empty());
}

#[test]
fn a_healthy_room_flags_nobody() {
    let healthy: Vec<Levels> = sep_fourth()
        .into_iter()
        .filter(|l| l.source != "geb")
        .collect();
    assert!(processed_with_evidence(&healthy).is_empty());
}

#[test]
fn the_evidence_travels_with_the_verdict() {
    // A bare source name would make the warning unarguable-with. The gap and the
    // sample size are what let a reader decide it is real.
    let found = processed_with_evidence(&sep_fourth());
    assert_eq!(found.len(), 1);
    let (source, gap, n) = &found[0];
    assert_eq!(source, "geb");
    assert!((*gap - 70.9).abs() < 0.1, "gap {gap}");
    assert_eq!(*n, 40);
}

#[test]
fn a_mic_that_is_merely_quiet_is_not_flagged() {
    // pixel9 is the widest-gapped REAL microphone in the room at 24.7 dB — the
    // closest any genuine mic comes to the threshold, and still 10 dB clear.
    assert!(processed_sources(&many("pixel9", -70.2, -94.9, 40)).is_empty());
}
