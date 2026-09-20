//! What the vocabulary prompt buys and what it costs, on short clips (#1665).
//!
//! The ASR prompt lists household names FIRST so Whisper spells them right, so
//! on audio it cannot place it reaches for them: measured over the archive, the
//! live tier is 8.8x likelier than the archive pass to emit a turn that is
//! nothing but a name. The obvious remedy — drop the prompt for very short
//! clips — trades one error for another, and #1665 says to measure BOTH
//! directions before choosing. This measures the harm direction.
//!
//! ⚠ **Known-truth audio containing NO names.** The committed public-domain
//! reading is cut to the lengths the live tier actually sends, so every
//! household name in the output is a hallucination by construction — there is
//! nothing to argue about.
//!
//!     RECALL_SYNC_TOKEN=… cargo run -p runner --example prompt_cost -- [<db>]
//!
//! ⚠ It prints COUNTS ONLY. The names are read from the archive to be searched
//! for and are never echoed, and neither is any transcript.

use audiocore::decode;
use audiocore::vad::RATE;
use runner::live::spoken;
use runner::shim::Shim;
use std::path::Path;

const FIXTURE: &str = "tests/fixtures/speech/public-domain-en.flac";
const ARCHIVE: &str = "/Volumes/Backup/recall/recall.sqlite";
/// The lengths live actually sends: its VAD fragments run 0.35-0.74 s, and the
/// archive pass sees a whole 60 s clip.
const LENGTHS: [f64; 5] = [0.5, 1.0, 2.0, 5.0, 12.0];

/// Enrolled household names, read to be SEARCHED FOR and never printed.
fn names(db: &Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(db).expect("open the archive");
    let mut stmt = conn
        .prepare(
            "SELECT name FROM speakers WHERE name <> '' \
             UNION SELECT DISTINCT speaker_label FROM transcript_segments \
             WHERE speaker_label IS NOT NULL AND speaker_label NOT LIKE 'SPEAKER%' \
               AND speaker_label <> ''",
        )
        .expect("prepare");
    stmt.query_map([], |r| r.get::<_, String>(0))
        .expect("query")
        .filter_map(Result::ok)
        .collect()
}

fn says_a_name(text: &str, names: &[String]) -> bool {
    let lower = text.to_lowercase();
    names
        .iter()
        .any(|n| lower.contains(&n.trim().to_lowercase()))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn cut(samples: &[f32], seconds: f64) -> Vec<&[f32]> {
    let per = (seconds * f64::from(RATE)) as usize;
    samples.chunks(per).filter(|c| c.len() == per).collect()
}

/// The quietest `want` fragments, which is where the harm actually lives.
///
/// ⚠ **Clearly read poetry is not the condition under test.** The archive
/// finding is that the tier reaches for a name on audio it CANNOT PLACE, and a
/// well-articulated stanza is placeable — so cutting the fixture evenly mostly
/// measures the easy case. The gaps between stanzas are the low-information
/// audio a half-second VAD fragment often really holds, and they are the arm
/// that should separate.
///
/// ⓘ Overlapping by a half-hop, so a 48-second fixture yields enough quiet
/// fragments to say anything at all.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn quietest(samples: &[f32], seconds: f64, want: usize) -> Vec<&[f32]> {
    let per = (seconds * f64::from(RATE)) as usize;
    let hop = per / 2;
    let mut ranked: Vec<(f64, &[f32])> = samples
        .windows(per)
        .step_by(hop.max(1))
        .map(|w| {
            let power: f64 = w.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
            (power, w)
        })
        .collect();
    ranked.sort_by(|a, b| a.0.total_cmp(&b.0));
    ranked.into_iter().take(want).map(|(_, w)| w).collect()
}

fn run(
    shim: &mut Shim,
    clips: &[&[f32]],
    prompt: Option<&str>,
    known: &[String],
) -> (usize, usize) {
    let mut said = 0;
    let mut reached = 0;
    for clip in clips {
        let file = tempfile::Builder::new()
            .suffix(".wav")
            .tempfile()
            .expect("scratch");
        audiocore::wav::write_mono16(file.path(), RATE, clip).expect("write");
        let result = shim
            .transcribe(file.path(), None, prompt)
            .expect("transcribe");
        if let Some((text, _)) = spoken(&result) {
            said += 1;
            if says_a_name(&text, known) {
                reached += 1;
            }
        }
    }
    (said, reached)
}

fn main() {
    let db = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ARCHIVE.to_owned());
    let names = names(Path::new(&db));
    assert!(!names.is_empty(), "no enrolled names to search for");
    println!(
        "searching for {} enrolled name(s); none are printed",
        names.len()
    );

    let pcm = decode::decode_s16(Path::new(FIXTURE), RATE).expect("decode");
    let samples = decode::to_f32(&pcm);

    let python = std::env::var("RECALL_PYTHON").unwrap_or_else(|_| ".venv/bin/python".to_owned());
    let mut shim = Shim::spawn(&python, &["-m".to_owned(), "recall.shim_asr".to_owned()])
        .expect("the asr shim");
    assert_eq!(shim.hello().expect("hello"), "asr");

    // ⚠ The glossary from the RUNNING fleet, so this measures the prompt that
    // actually ships rather than a reconstruction of it.
    // ⚠ The sync token from the environment, never a literal: this reads the
    // household's real glossary and the credential must not live in the repo.
    let token = std::env::var("RECALL_SYNC_TOKEN")
        .expect("RECALL_SYNC_TOKEN must be set — the glossary is behind the sync plane");
    let client = runner::client::Client::new("http://10.100.0.2:8001", &token);
    let prompt = client
        .prompt("http://10.100.0.2:8000")
        .expect("the household glossary")
        .expect("a non-empty glossary");
    println!("prompt is {} chars; not printed\n", prompt.chars().count());

    println!(
        "{:>7}  {:>6}  {:>22}  {:>22}",
        "length", "clips", "named WITH prompt", "named WITHOUT"
    );
    for seconds in LENGTHS {
        let clips = cut(&samples, seconds);
        let (with_said, with_named) = run(&mut shim, &clips, Some(&prompt), &names);
        let (without_said, without_named) = run(&mut shim, &clips, None, &names);
        println!(
            "{seconds:>6.1}s  {:>6}  {:>10} of {:<9}  {:>10} of {:<9}",
            clips.len(),
            with_named,
            with_said,
            without_named,
            without_said
        );
    }

    // ⚠ The condition the archive finding is actually about: audio the model
    // cannot place. An evenly cut reading is mostly the easy case.
    println!(
        "\nthe QUIETEST fragments — low-information audio, which is what a\n              half-second VAD fragment often really holds:"
    );
    println!(
        "{:>7}  {:>6}  {:>22}  {:>22}",
        "length", "clips", "named WITH prompt", "named WITHOUT"
    );
    for seconds in [0.5, 1.0] {
        let clips = quietest(&samples, seconds, 120);
        let (with_said, with_named) = run(&mut shim, &clips, Some(&prompt), &names);
        let (without_said, without_named) = run(&mut shim, &clips, None, &names);
        println!(
            "{seconds:>6.1}s  {:>6}  {:>10} of {:<9}  {:>10} of {:<9}",
            clips.len(),
            with_named,
            with_said,
            without_named,
            without_said
        );
    }
    println!("\nthe reading contains NO household name, so every count above is a hallucination");
}
