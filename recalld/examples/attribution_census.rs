//! Attribution accuracy, with the POPULATION STATED — #1470's open question.
//!
//! Two numbers existed and disagreed (0.9138 and 0.4962) with no way to tell
//! which described the system. This runs the PRODUCTION rule
//! (`identify::match_one`, the same code the fleet attributes with) over every
//! human-labelled turn that carries a stored embedding, and reports each
//! population separately instead of one headline.
//!
//! Input is two `|`-separated dumps from the fleet's `recall.sqlite`, because
//! the analysis belongs where the production rule is and the data does not need
//! copying wholesale:
//! ```text
//!   prints.psv  person|source_segment_id|vector
//!   turns.psv   id|label|seconds|visible|current|_|vector
//! ```
//!
//! ⚠ **The exclusion is LEAVE-ONE-OUT ON PROVENANCE, and getting it wrong is the
//! whole trap.** Every enrolled print is derived from a human-labelled TURN
//! (`speaker_embeddings.source_segment_id` -> `transcript_segments.id`; all 798
//! resolve there). 240 of the 522 scored turns are themselves a print's source,
//! so scoring them against the full corpus asks the matcher to recognise a voice
//! from a vector made out of that very clip. A first pass excluded by comparing
//! VECTOR STRINGS, which catches none of it — the print is a different vector
//! from the same audio — and reported 1.0000 across three duration bands.
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

    // `leave_out`: drop every print derived from the turn being scored. The
    // person keeps their OTHER prints, so this asks the question that matters —
    // would the system name this turn right if it had not been enrolled from it.
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

/// The gradient that explains the two numbers: the population with stored
/// embeddings is long turns, and accuracy rises steeply with length.
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
