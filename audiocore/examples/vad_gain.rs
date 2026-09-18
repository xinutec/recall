//! How loud a stored segment is, how far the detector lifts it, and how much
//! speech it then hears. Compare against a peer's same minute (#1485).

use audiocore::decode;
use audiocore::vad::{Detector, Region, detection_gain};
use std::path::Path;

fn main() {
    let mut det = Detector::load().expect("detector");
    for arg in std::env::args().skip(1) {
        let path = Path::new(&arg);
        let Some(pcm) = decode::decode_s16(path, 16_000) else {
            println!("{arg}: undecodable");
            continue;
        };
        let samples = decode::to_f32(&pcm);
        let peak = samples.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        let heard: f64 = det
            .regions(&samples)
            .expect("detector")
            .iter()
            .map(Region::seconds)
            .sum();
        println!(
            "{}: peak {:.1} dBFS   lifted x{:.0}   speech {heard:.1}s",
            path.file_name().unwrap_or_default().to_string_lossy(),
            20.0 * peak.log10(),
            detection_gain(peak),
        );
    }
}
