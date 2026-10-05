//! What the vocabulary prompt costs on short clips: names put into audio that
//! has none (#1665). `prompt_spelling` measures what it buys.
//!
//! The prompt lists household names so Whisper spells them right, and on audio
//! it cannot place, Whisper reaches for them: over the archive, the live tier
//! was 8.8x likelier than the archive pass to emit a turn that is only a name.
//!
//! The audio is the committed public-domain reading, cut to the lengths the
//! live tier sends. It contains no household name, so every one in the output
//! is a hallucination.
//!
//!     RECALL_SYNC_TOKEN=… cargo run -p runner --example prompt_cost -- [<db>]
//!
//! Prints counts only, never a name or a transcript.

use audiocore::decode;
use audiocore::vad::RATE;
use clap::Parser;
use runner::live::spoken;
use runner::shim::Shim;
use std::path::Path;

const FIXTURE: &str = "tests/fixtures/speech/public-domain-en.flac";
const ARCHIVE: &str = "/Volumes/Backup/recall/recall.sqlite";
/// Clip lengths to try, from the live tier's typical fragment up to its
/// longest call (`live::CALL_SECONDS`).
const LENGTHS: [f64; 5] = [0.5, 1.0, 2.0, 5.0, 12.0];

/// The enrolled names, to search for.
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

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "example sizes: seconds of audio, far inside range"
)]
fn cut(samples: &[f32], seconds: f64) -> Vec<&[f32]> {
    let per = (seconds * f64::from(RATE)) as usize;
    samples.chunks(per).filter(|c| c.len() == per).collect()
}

/// The quietest `want` fragments, overlapping by half so a 48-second fixture
/// yields enough of them.
///
/// Clearly read verse is easy to place; the hallucination happens on audio the
/// model cannot place. The gaps between stanzas are that kind of audio, like
/// many half-second VAD fragments.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "example sizes: seconds of audio, far inside range"
)]
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
        if let Some((text, _)) = spoken(&result.reply) {
            said += 1;
            if says_a_name(&text, known) {
                reached += 1;
            }
        }
    }
    (said, reached)
}

/// What the vocabulary prompt buys and what it costs, on short clips (#1665).
#[derive(Parser)]
struct Cli {
    /// The archive's database, for the enrolled names.
    #[arg(default_value = ARCHIVE)]
    archive: String,
}

fn main() {
    let db = Cli::parse().archive;
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

    // The prompt that ships, from the running server.
    let token = std::env::var("RECALL_SYNC_TOKEN")
        .expect("RECALL_SYNC_TOKEN must be set — the glossary is behind the sync plane");
    let client = runner::client::Client::new("https://recall.xinutec.org", &token);
    let prompt = client
        .prompt()
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
