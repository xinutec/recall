//! Short-time Fourier analysis/synthesis for the fusion engine: Hann-windowed
//! frames at 50% overlap, with exact reconstruction by accumulating the
//! squared synthesis window rather than assuming a COLA constant — correct by
//! construction, whatever the frame size.

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

/// One frame's spectrum: `size/2 + 1` bins (real input, positive frequencies).
pub type Spectrum = Vec<Complex<f32>>;

pub struct Stft {
    size: usize,
    hop: usize,
    window: Vec<f32>,
    forward: Arc<dyn Fft<f32>>,
    inverse: Arc<dyn Fft<f32>>,
}

impl Stft {
    /// `size` must be even; `hop` = `size / 2` (50% overlap Hann, the one
    /// configuration this engine uses).
    pub fn new(size: usize) -> Self {
        assert!(size.is_multiple_of(2), "frame size must be even");
        let mut planner = FftPlanner::new();
        let window = (0..size)
            .map(|n| {
                let x = std::f32::consts::TAU * n as f32 / size as f32;
                0.5 * (1.0 - x.cos())
            })
            .collect();
        Self {
            size,
            hop: size / 2,
            window,
            forward: planner.plan_fft_forward(size),
            inverse: planner.plan_fft_inverse(size),
        }
    }

    pub fn bins(&self) -> usize {
        self.size / 2 + 1
    }

    /// Analyse `samples` into windowed frames of positive-frequency bins.
    pub fn analyse(&self, samples: &[f32]) -> Vec<Spectrum> {
        let mut frames = Vec::new();
        let mut buf = vec![Complex::new(0.0f32, 0.0); self.size];
        let mut start = 0;
        while start + self.size <= samples.len() {
            for (i, slot) in buf.iter_mut().enumerate() {
                *slot = Complex::new(samples[start + i] * self.window[i], 0.0);
            }
            self.forward.process(&mut buf);
            frames.push(buf[..self.bins()].to_vec());
            start += self.hop;
        }
        frames
    }

    /// Overlap-add synthesis, dividing by the accumulated squared window so
    /// analysis+synthesis is the identity wherever the window covered.
    pub fn synthesise(&self, frames: &[Spectrum]) -> Vec<f32> {
        if frames.is_empty() {
            return Vec::new();
        }
        let out_len = (frames.len() - 1) * self.hop + self.size;
        let mut out = vec![0.0f32; out_len];
        let mut norm = vec![0.0f32; out_len];
        let mut buf = vec![Complex::new(0.0f32, 0.0); self.size];
        for (index, frame) in frames.iter().enumerate() {
            // Restore conjugate symmetry for the real inverse.
            buf[..self.bins()].copy_from_slice(frame);
            for i in self.bins()..self.size {
                buf[i] = frame[self.size - i].conj();
            }
            self.inverse.process(&mut buf);
            let start = index * self.hop;
            for i in 0..self.size {
                // rustfft's inverse is unnormalised: divide by size here.
                let sample = buf[i].re / self.size as f32;
                out[start + i] += sample * self.window[i];
                norm[start + i] += self.window[i] * self.window[i];
            }
        }
        for (sample, n) in out.iter_mut().zip(&norm) {
            if *n > f32::EPSILON {
                *sample /= *n;
            }
        }
        out
    }
}
