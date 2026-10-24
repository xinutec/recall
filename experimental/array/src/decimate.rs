//! 48 kHz to 16 kHz, what speech-to-text consumes: a windowed-sinc low-pass
//! below the new Nyquist, then every third sample.

/// Half the filter's length, in input samples.
const HALF: isize = 96;
/// The pass band ends here, below 8 kHz with room for the transition.
const CUTOFF_HZ: f64 = 7_200.0;

pub fn to_16k(x: &[f32]) -> Vec<f32> {
    let fc = CUTOFF_HZ / 48_000.0;
    let taps: Vec<f64> = (-HALF..=HALF)
        .map(|n| {
            let t = n as f64;
            let sinc = if n == 0 {
                2.0 * fc
            } else {
                (std::f64::consts::TAU * fc * t).sin() / (std::f64::consts::PI * t)
            };
            // Blackman window.
            let w = 0.42
                + 0.5 * (std::f64::consts::PI * t / HALF as f64).cos()
                + 0.08 * (std::f64::consts::TAU * t / HALF as f64).cos();
            sinc * w
        })
        .collect();
    (0..x.len() / 3)
        .map(|j| {
            let c = (3 * j) as isize;
            let mut v = 0.0;
            for (i, h) in taps.iter().enumerate() {
                let k = c + i as isize - HALF;
                if k >= 0 && (k as usize) < x.len() {
                    v += f64::from(x[k as usize]) * h;
                }
            }
            v as f32
        })
        .collect()
}
