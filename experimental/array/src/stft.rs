//! A short-time Fourier transform with frames long enough to hold a room's
//! echo (171 ms), and its exact inverse: square-root Hann windows at 75%
//! overlap, whose squares sum to a constant.

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

/// Samples per frame: 171 ms at 48 kHz.
pub const FRAME: usize = 8192;
/// Samples between frame starts.
pub const HOP: usize = FRAME / 4;
/// Zeros before the signal, so its first samples are covered by full overlap.
const PAD: usize = FRAME;
/// The squared windows of overlapping frames sum to this.
const OVERLAP_SUM: f64 = 2.0;

/// One frame's spectrum, bins 0..=FRAME/2.
pub type Spectrum = Vec<Complex<f64>>;

fn window() -> Vec<f64> {
    (0..FRAME)
        .map(|n| (0.5 - 0.5 * (std::f64::consts::TAU * n as f64 / FRAME as f64).cos()).sqrt())
        .collect()
}

/// The sample a frame is centred on, in the input's own indices.
pub fn centre(frame: usize) -> f64 {
    (frame * HOP + FRAME / 2) as f64 - PAD as f64
}

pub fn stft(x: &[f32]) -> Vec<Spectrum> {
    let w = window();
    let fft = FftPlanner::<f64>::new().plan_fft_forward(FRAME);
    let frames = (x.len() + 2 * PAD - FRAME) / HOP + 1;
    (0..frames)
        .map(|f| {
            let mut buf: Vec<Complex<f64>> = (0..FRAME)
                .map(|n| {
                    let i = (f * HOP + n) as isize - PAD as isize;
                    let v = if i >= 0 && (i as usize) < x.len() {
                        f64::from(x[i as usize])
                    } else {
                        0.0
                    };
                    Complex::new(v * w[n], 0.0)
                })
                .collect();
            fft.process(&mut buf);
            buf.truncate(FRAME / 2 + 1);
            buf
        })
        .collect()
}

/// The signal of `len` samples whose spectra these are.
pub fn istft(frames: &[Spectrum], len: usize) -> Vec<f32> {
    let w = window();
    let ifft = FftPlanner::<f64>::new().plan_fft_inverse(FRAME);
    let mut out = vec![0.0f64; len + 2 * PAD + FRAME];
    for (f, half) in frames.iter().enumerate() {
        let mut buf = vec![Complex::new(0.0, 0.0); FRAME];
        buf[..half.len()].copy_from_slice(half);
        for k in 1..FRAME / 2 {
            buf[FRAME - k] = half[k].conj();
        }
        ifft.process(&mut buf);
        for n in 0..FRAME {
            out[f * HOP + n] += buf[n].re / FRAME as f64 * w[n] / OVERLAP_SUM;
        }
    }
    out[PAD..PAD + len].iter().map(|v| *v as f32).collect()
}
