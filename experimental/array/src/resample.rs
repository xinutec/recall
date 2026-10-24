//! A mic's samples read at the instants of another clock, by windowed-sinc
//! interpolation: each device samples on its own clock, and the combiner needs
//! them on one.

const TAPS: isize = 8;

fn lanczos(t: f64) -> f64 {
    if t.abs() < 1e-12 {
        return 1.0;
    }
    if t.abs() >= TAPS as f64 {
        return 0.0;
    }
    let pt = std::f64::consts::PI * t;
    TAPS as f64 * pt.sin() * (pt / TAPS as f64).sin() / (pt * pt)
}

/// `len` samples: sample `i` is `mic` read at (fractional) index `at(i)`;
/// zero where that falls outside it.
pub fn onto(mic: &[f32], at: impl Fn(f64) -> f64, len: usize) -> Vec<f32> {
    (0..len)
        .map(|i| {
            let t = at(i as f64);
            let k0 = t.floor() as isize;
            let mut v = 0.0;
            for k in (k0 - TAPS + 1)..=(k0 + TAPS) {
                if k >= 0 && (k as usize) < mic.len() {
                    v += f64::from(mic[k as usize]) * lanczos(t - k as f64);
                }
            }
            v as f32
        })
        .collect()
}
