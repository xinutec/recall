use audiod::align::best_lag;
use audiod::envelope::{BUCKET_S, DECODE_RATE, rms_buckets};

/// A speech-shaped envelope: quiet floor with irregular bursts.
fn bursty(len: usize, seed: u64) -> Vec<f32> {
    let mut v = vec![0.01f32; len];
    let mut state = seed;
    let mut i = 0;
    while i < len {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let gap = 3 + (state >> 33) as usize % 20;
        let burst = 2 + (state >> 40) as usize % 8;
        let end = (i + gap + burst).min(len);
        for (j, slot) in v.iter_mut().enumerate().take(end).skip(i + gap) {
            *slot = 0.2 + ((state >> (j % 17)) & 0xf) as f32 / 40.0;
        }
        i += gap + burst;
    }
    v
}

#[test]
fn a_late_stamped_copy_measures_a_negative_offset() {
    // The June phone case: the same room, stamped d buckets LATE on the
    // nominal timeline. The anchor must say "add a negative offset".
    let reference = bursty(600, 7);
    let d = 39; // 3.9 s in buckets — the measured June skew
    let mut late = vec![0.01f32; 600];
    late[d..].copy_from_slice(&reference[..600 - d]);
    let anchor = best_lag(&reference, &late, 150, BUCKET_S, 300).expect("verdict");
    assert!((anchor.offset_s - (-(d as f64) * BUCKET_S)).abs() < 1e-9);
    assert!(anchor.peak_r > 0.95, "peak_r = {}", anchor.peak_r);
}

#[test]
fn an_aligned_copy_measures_zero() {
    let reference = bursty(600, 11);
    let anchor = best_lag(&reference, &reference.clone(), 150, BUCKET_S, 300).expect("verdict");
    assert!(anchor.offset_s.abs() < 1e-9);
    assert!(anchor.peak_r > 0.999);
}

#[test]
fn different_rooms_correlate_weakly() {
    let a = bursty(600, 13);
    let b = bursty(600, 101);
    let anchor = best_lag(&a, &b, 150, BUCKET_S, 300).expect("verdict");
    assert!(anchor.peak_r < 0.5, "peak_r = {}", anchor.peak_r);
}

#[test]
fn too_little_overlap_refuses_a_verdict() {
    let a = bursty(100, 17);
    assert!(best_lag(&a, &a.clone(), 150, BUCKET_S, 300).is_none());
}

#[test]
fn envelope_buckets_measure_rms_at_the_declared_rate() {
    let samples_per_bucket = (f64::from(DECODE_RATE) * BUCKET_S) as usize;
    // One bucket of full-scale square wave, one of silence, half a bucket dropped.
    let mut pcm = Vec::new();
    for _ in 0..samples_per_bucket {
        pcm.extend_from_slice(&i16::MAX.to_le_bytes());
    }
    pcm.extend(std::iter::repeat_n(0u8, 2 * samples_per_bucket));
    pcm.extend(std::iter::repeat_n(1u8, samples_per_bucket)); // partial: dropped
    let env = rms_buckets(&pcm);
    assert_eq!(env.len(), 2);
    assert!((env[0] - 1.0).abs() < 1e-3);
    assert!(env[1] < 1e-6);
}
