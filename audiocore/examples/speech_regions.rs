//! Where the detector hears speech in stored segments: one line per path read
//! from stdin, `path<TAB>[[start,end],...]`, `undecodable` if it could not look.

use audiocore::vad::Detector;
use std::io::BufRead as _;
use std::path::Path;

fn main() {
    let mut det = Detector::load().expect("detector");
    for line in std::io::stdin().lock().lines() {
        let path = line.expect("stdin");
        match det.speech_regions(Path::new(&path)) {
            Ok(regions) => {
                let spans: Vec<[f64; 2]> = regions.iter().map(|r| [r.start, r.end]).collect();
                println!("{path}\t{}", serde_json::to_string(&spans).expect("json"));
            }
            Err(_) => println!("{path}\tundecodable"),
        }
    }
}
