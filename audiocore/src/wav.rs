//! Minimal mono 16-bit WAV writing — the fusion prototypes' output format,
//! because the ASR working-copy path accepts WAV directly and 44 bytes of
//! header is not worth a dependency.

use std::io::Write;
use std::path::Path;

pub fn write_mono16(path: &Path, rate: u32, samples: &[f32]) -> std::io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    out.write_all(b"RIFF")?;
    out.write_all(&(36 + data_len).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?; // PCM chunk size
    out.write_all(&1u16.to_le_bytes())?; // PCM
    out.write_all(&1u16.to_le_bytes())?; // mono
    out.write_all(&rate.to_le_bytes())?;
    out.write_all(&(rate * 2).to_le_bytes())?; // byte rate
    out.write_all(&2u16.to_le_bytes())?; // block align
    out.write_all(&16u16.to_le_bytes())?; // bits
    out.write_all(b"data")?;
    out.write_all(&data_len.to_le_bytes())?;
    for sample in samples {
        let s = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
        out.write_all(&s.to_le_bytes())?;
    }
    out.flush()
}
