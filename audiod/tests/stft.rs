use audiod::stft::Stft;

#[test]
fn analysis_then_synthesis_is_the_identity() {
    let stft = Stft::new(512);
    // A deterministic mix of tones — enough structure to expose any window
    // or normalisation error at once.
    let samples: Vec<f32> = (0..16_000)
        .map(|n| {
            let t = n as f32 / 16_000.0;
            0.4 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()
                + 0.2 * (2.0 * std::f32::consts::PI * 1330.0 * t).sin()
        })
        .collect();
    let rebuilt = stft.synthesise(&stft.analyse(&samples));
    // Compare where full window overlap exists (skip the first/last frame).
    let (a, b) = (
        &samples[512..rebuilt.len() - 512],
        &rebuilt[512..rebuilt.len() - 512],
    );
    let worst = a
        .iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(worst < 1e-4, "worst reconstruction error {worst}");
}
