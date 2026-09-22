//! Does a gate survive the archive — lossy coding, then the 16 kHz decode the
//! level scanner uses? Prints the longest exact-zero and near-zero run at native
//! rate and at 16 kHz.
//!
//! Exact zeros do not survive lossy coding; near-zero runs do (#1526).

use audiocore::decode;
use std::path::Path;

fn longest_run(x: &[i16], limit: i16) -> usize {
    let (mut best, mut run) = (0usize, 0usize);
    for &s in x {
        if s.abs() <= limit {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

fn s16(pcm: &[u8]) -> Vec<i16> {
    pcm.as_chunks::<2>()
        .0
        .iter()
        .map(|p| i16::from_le_bytes(*p))
        .collect()
}

fn main() {
    for arg in std::env::args().skip(1) {
        let path = Path::new(&arg);
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let Some((native_rate, _)) = decode::stream_shape(path) else {
            println!("{name}: unreadable");
            continue;
        };
        for (label, rate, pcm) in [
            ("native", native_rate, decode::decode_native_s16(path)),
            ("16k", 16_000, decode::decode_s16(path, 16_000)),
        ] {
            let Some(pcm) = pcm else { continue };
            let x = s16(&pcm);
            if x.is_empty() {
                continue;
            }
            let rate = rate as f64;
            let zeros = x.iter().filter(|s| **s == 0).count() as f64 / x.len() as f64;
            println!(
                "{name:34} {label:6} {rate:6.0}Hz  exact-zero {:5.1}%  \
                 longest zero run {:6.3}s   longest <=2 LSB run {:6.3}s",
                zeros * 100.0,
                longest_run(&x, 0) as f64 / rate,
                longest_run(&x, 2) as f64 / rate,
            );
        }
    }
}
