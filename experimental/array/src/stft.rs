//! A short-time Fourier transform with frames long enough to hold a room's
//! echo (171 ms), and its exact inverse: square-root Hann windows at 75%
//! overlap, whose squares sum to a constant.

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

/// Samples per frame: 171 ms at 48 kHz.
pub const FRAME: usize = 8192;
/// Samples between frame starts.
pub const HOP: usize = FRAME / 4;

/// The squared windows of overlapping frames sum to this.
const OVERLAP_SUM: f64 = 2.0;

/// One frame's spectrum, bins 0..=FRAME/2.
pub type Spectrum = Vec<Complex<f64>>;

fn window(frame: usize) -> Vec<f64> {
    (0..frame)
        .map(|n| (0.5 - 0.5 * (std::f64::consts::TAU * n as f64 / frame as f64).cos()).sqrt())
        .collect()
}

/// The sample a [`FRAME`]-long frame is centred on, in the input's own indices.
pub fn centre(frame: usize) -> f64 {
    (frame * HOP + FRAME / 2) as f64 - FRAME as f64
}

/// [`stft_with`] at [`FRAME`].
pub fn stft(x: &[f32]) -> Vec<Spectrum> {
    stft_with(x, FRAME)
}

/// [`istft_with`] at [`FRAME`].
pub fn istft(frames: &[Spectrum], len: usize) -> Vec<f32> {
    istft_with(frames, len, FRAME)
}

/// Frames of `frame` samples every `frame / 4`, the signal padded by a frame of
/// zeros at each end so its edges get full overlap.
pub fn stft_with(x: &[f32], frame: usize) -> Vec<Spectrum> {
    let (hop, pad) = (frame / 4, frame);
    let w = window(frame);
    let fft = FftPlanner::<f64>::new().plan_fft_forward(frame);
    let frames = (x.len() + 2 * pad - frame) / hop + 1;
    (0..frames)
        .map(|f| {
            let mut buf: Vec<Complex<f64>> = (0..frame)
                .map(|n| {
                    let i = (f * hop + n) as isize - pad as isize;
                    let v = if i >= 0 && (i as usize) < x.len() {
                        f64::from(x[i as usize])
                    } else {
                        0.0
                    };
                    Complex::new(v * w[n], 0.0)
                })
                .collect();
            fft.process(&mut buf);
            buf.truncate(frame / 2 + 1);
            buf
        })
        .collect()
}

/// The signal of `len` samples whose `frame`-sample spectra these are.
pub fn istft_with(frames: &[Spectrum], len: usize, frame: usize) -> Vec<f32> {
    let (hop, pad) = (frame / 4, frame);
    let w = window(frame);
    let ifft = FftPlanner::<f64>::new().plan_fft_inverse(frame);
    let mut out = vec![0.0f64; len + 2 * pad + frame + frames.len() * hop];
    for (f, half) in frames.iter().enumerate() {
        let mut buf = vec![Complex::new(0.0, 0.0); frame];
        buf[..half.len()].copy_from_slice(half);
        for k in 1..frame / 2 {
            buf[frame - k] = half[k].conj();
        }
        ifft.process(&mut buf);
        for n in 0..frame {
            out[f * hop + n] += buf[n].re / frame as f64 * w[n] / OVERLAP_SUM;
        }
    }
    out[pad..pad + len].iter().map(|v| *v as f32).collect()
}
