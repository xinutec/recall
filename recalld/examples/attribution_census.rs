//! Attribution accuracy with the population stated (#1470).
//!
//! Runs the production rule over every human-labelled turn with a stored
//! embedding, reporting each population separately rather than one headline.
//!
//! Exclusion is LEAVE-ONE-OUT ON PROVENANCE: every print derives from a labelled
//! turn (`speaker_embeddings.source_segment_id`), and 240 of 522 scored turns are
//! a print's own source. Excluding by comparing VECTORS catches none of them — an
//! enrolled print is a different vector from the same audio — and reports 1.0000.
//!
//! Input: two `|`-separated dumps from the fleet's `recall.sqlite`:
//! ```text
//!   prints.psv  person|source_segment_id|vector
//!   turns.psv   id|label|seconds|visible|current|_|vector
//! ```

use recalld::identify::{Voiceprint, match_one};
use std::collections::BTreeMap;

struct Turn {
    id: i64,
    label: String,
    seconds: f64,
    visible: bool,
    current: bool,
    vector: Vec<f64>,
}

/// A print, with the turn it was made from.
struct Print {
    print: Voiceprint,
    source_turn: i64,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (prints_path, turns_path) = (&args[0], &args[1]);

    let prints: Vec<Print> = std::fs::read_to_string(prints_path)
        .expect("prints")
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.splitn(3, '|').collect();
            if f.len() < 3 {
                return None;
            }
            Some(Print {
                print: Voiceprint {
                    person: f[0].to_owned(),
                    vector: serde_json::from_str(f[2]).ok()?,
                },
                source_turn: f[1].parse().unwrap_or(-1),
            })
        })
        .collect();

    let turns: Vec<Turn> = std::fs::read_to_string(turns_path)
        .expect("turns")
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.splitn(7, '|').collect();
            if f.len() < 7 {
                return None;
            }
            Some(Turn {
                id: f[0].parse().ok()?,
                label: f[1].to_owned(),
                seconds: f[2].parse().ok()?,
                visible: f[3] == "visible",
                current: f[4] == "current",
                vector: serde_json::from_str(f[6]).ok()?,
            })
        })
        .collect();

    let people: std::collections::BTreeSet<&str> =
        prints.iter().map(|p| p.print.person.as_str()).collect();
    let sourced: std::collections::BTreeSet<i64> = prints.iter().map(|p| p.source_turn).collect();
    println!(
        "{} enrolled voiceprints over {} people; {} labelled turns with a stored embedding\n",
        prints.len(),
        people.len(),
        turns.len()
    );

    // `leave_out` drops prints derived from the turn being scored; the person
    // keeps their others, so this asks whether the turn would be named right had
    // it not been enrolled from.
    let score = |subset: &[&Turn], leave_out: bool| -> (usize, f64) {
        let hit = subset
            .iter()
            .filter(|t| {
                let corpus: Vec<Voiceprint> = prints
                    .iter()
                    .filter(|p| !leave_out || p.source_turn != t.id)
                    .map(|p| p.print.clone())
                    .collect();
                match_one(&t.vector, &corpus).is_some_and(|guess| guess.person == t.label)
            })
            .count();
        let n = subset.len();
        (n, if n == 0 { 0.0 } else { hit as f64 / n as f64 })
    };

    let all: Vec<&Turn> = turns.iter().collect();
    let enrolled_from: Vec<&Turn> = turns.iter().filter(|t| sourced.contains(&t.id)).collect();
    let never_enrolled: Vec<&Turn> = turns.iter().filter(|t| !sourced.contains(&t.id)).collect();
    let honest: Vec<&Turn> = turns.iter().filter(|t| t.visible && t.current).collect();

    println!("{:50} {:>5} {:>8}", "population", "n", "accuracy");
    let rows: [(&str, &Vec<&Turn>, bool); 6] = [
        (
            "EVERY labelled turn, FULL corpus (contaminated)",
            &all,
            false,
        ),
        ("EVERY labelled turn, leave-one-out", &all, true),
        (
            "  turns a print was made FROM, full corpus",
            &enrolled_from,
            false,
        ),
        ("  the same turns, leave-one-out", &enrolled_from, true),
        ("  turns NO print was made from", &never_enrolled, false),
        ("visible + current, leave-one-out", &honest, true),
    ];
    for (name, subset, leave_out) in rows {
        let (n, acc) = score(subset, leave_out);
        println!("{name:50} {n:>5} {acc:>8.4}");
    }

    by_length(&turns, &score);
}

/// Accuracy rises steeply with turn length, which is what separated the two
/// disagreeing numbers.
fn by_length(turns: &[Turn], score: &impl Fn(&[&Turn], bool) -> (usize, f64)) {
    println!("\nvisible + current + leave-one-out, by turn length:");
    let mut bands: BTreeMap<&str, Vec<&Turn>> = BTreeMap::new();
    for t in turns.iter().filter(|t| t.visible && t.current) {
        let band = match t.seconds {
            s if s < 1.0 => "a <1s",
            s if s < 2.0 => "b 1-2s",
            s if s < 5.0 => "c 2-5s",
            s if s < 10.0 => "d 5-10s",
            _ => "e >10s",
        };
        bands.entry(band).or_default().push(t);
    }
    for (band, subset) in bands {
        let (n, acc) = score(&subset, true);
        println!("  {:48} {n:>5} {acc:>8.4}", &band[2..]);
    }
}
