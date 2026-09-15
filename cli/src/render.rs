//! Turning API turns into the lines a person reads. Pure — no network, no clock
//! beyond the local offset — so every rule below is unit-tested directly.
//!
//! ⚠ **This is a PORT of `recall.transcript_view`**, and the rules it carries
//! are display decisions that were made once and should not be re-made by
//! accident: which attribution a search hit shows versus a read-through
//! transcript, what an unscored guess looks like, and the audibility bands. Each
//! is stated where it is applied, with the test that pins it.

use crate::api::Turn;
use chrono::{DateTime, Local};

/// Audibility bands from measured loudness, as the labelling UI draws them
/// (`train.ts`). Two copies of two numbers; the UI's are the original.
const CLEAR_LOUDNESS: f64 = 0.05;
const QUIET_LOUDNESS: f64 = 0.01;

/// The archive stores UTC; a person asking "when" means the wall-clock the
/// speech happened on. Every rendered instant goes through here.
///
/// An unparseable instant is shown verbatim rather than dropped — a turn with a
/// malformed timestamp is a thing worth seeing, not hiding.
fn local(iso: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|t| t.with_timezone(&Local))
}

fn when(iso: &str, format: &str) -> String {
    local(iso).map_or_else(|| iso.to_owned(), |t| t.format(format).to_string())
}

/// Who said a turn, for a SEARCH HIT.
///
/// A human-confirmed name is authoritative; otherwise the voiceprint guess with
/// its strength as a hint ("Pippijn ~76%"); otherwise the bare diarization
/// voice; otherwise "unknown".
///
/// ⚠ Deliberately more speculative than [`who`]. Most search hits are
/// unconfirmed machine turns, so the guess is the only signal there is — hiding
/// it would make search answer "unknown" to nearly everything.
#[must_use]
pub fn attribution(turn: &Turn) -> String {
    if turn.speaker_confirmed
        && let Some(name) = &turn.speaker
    {
        return name.clone();
    }
    if let Some(guess) = &turn.speaker {
        let strength = turn
            .speaker_confidence
            .map_or_else(String::new, |s| format!(" ~{}%", (s * 100.0).round()));
        return format!("{guess}{strength}");
    }
    turn.cluster.clone().unwrap_or_else(|| "unknown".to_owned())
}

/// Who said a turn, for a READ-THROUGH TRANSCRIPT: a confirmed name, else the
/// diarization voice so distinct unnamed speakers stay distinguishable, else
/// "unknown". No speculation — a transcript someone reads end to end should not
/// assert a name the machine only guessed.
#[must_use]
pub fn who(turn: &Turn) -> String {
    if turn.speaker_confirmed
        && let Some(name) = &turn.speaker
    {
        return name.clone();
    }
    turn.cluster.clone().unwrap_or_else(|| "unknown".to_owned())
}

/// One search hit: when, who, language, source, text.
#[must_use]
pub fn hit(turn: &Turn) -> String {
    let lang = turn
        .language
        .as_ref()
        .map_or_else(String::new, |l| format!(" [{l}]"));
    let src = turn
        .source
        .as_ref()
        .map_or_else(String::new, |s| format!(" ({s})"));
    format!(
        "{}  {}{lang}{src}  {}",
        when(&turn.start, "%Y-%m-%d %H:%M:%S"),
        attribution(turn),
        turn.text
    )
}

/// The audibility band a measured loudness falls in.
fn clarity(loudness: Option<f64>) -> &'static str {
    match loudness {
        None => "unmeasured",
        Some(l) if l >= CLEAR_LOUDNESS => "clear",
        Some(l) if l >= QUIET_LOUDNESS => "quiet",
        Some(_) => "faint",
    }
}

/// Attribution for the diagnostic view, tagged with how it was decided.
///
/// ⚠ A confirmed label that is still `SPEAKER_*` is not a name — it is
/// diarization's own placeholder that happened to get written to the label
/// column — so it does not count as confirmed here.
fn who_detail(turn: &Turn) -> String {
    match &turn.speaker {
        Some(name) if turn.speaker_confirmed && !name.starts_with("SPEAKER_") => {
            format!("{name} (confirmed)")
        }
        Some(guess) if !turn.speaker_confirmed => {
            let pct = turn
                .speaker_confidence
                .map_or_else(String::new, |s| format!(" ~{}%", (s * 100.0).round()));
            format!("{guess}{pct} (guess)")
        }
        _ => "unknown".to_owned(),
    }
}

/// A diagnostic dump of specific turns — every field that explains a turn's
/// state. For inspecting one by id rather than reading a session.
///
/// ⚠ **`asked` is what the caller typed, and it is printed when it differs from
/// what came back.** `/api/transcripts` follows supersession to the CURRENT
/// version of a turn, so asking about a corrected turn's old id answers about
/// its replacement. The Python this replaces read the row itself and printed
/// "superseded by #N"; saying which id actually answered is the same fact from
/// the other end, and the alternative — printing the new turn under the old
/// number — is how a person concludes their correction never applied.
#[must_use]
pub fn details(asked: &[i64], turns: &[Turn]) -> String {
    turns
        .iter()
        .zip(asked.iter().copied().chain(std::iter::repeat(0)))
        .map(|(t, asked_id)| {
            // A turn is seconds long, so the millisecond count is nowhere near
            // f64's exact-integer range; the cast is stated rather than left to
            // be noticed.
            #[allow(clippy::cast_precision_loss)]
            let duration = local(&t.end)
                .zip(local(&t.start))
                .map_or(0.0, |(e, s)| (e - s).num_milliseconds() as f64 / 1000.0);
            let status = match &t.hidden {
                Some(why) => format!("hidden: {why}"),
                None if asked_id != 0 && asked_id != t.id => {
                    format!("current; #{asked_id} was superseded by this")
                }
                None => "visible".to_owned(),
            };
            let conf = t
                .confidence
                .map_or_else(|| "—".to_owned(), |c| format!("{c:.2}"));
            let loud = t
                .loudness
                .map_or_else(|| "—".to_owned(), |l| format!("{l:.5}"));
            [
                format!(
                    "#{}  {}-{}  ({duration:.1}s)  [{}]  src={}",
                    t.id,
                    when(&t.start, "%a %d %b %Y %H:%M:%S"),
                    when(&t.end, "%H:%M:%S"),
                    t.language.as_deref().unwrap_or("?"),
                    t.source.as_deref().unwrap_or("—"),
                ),
                format!(
                    "  who      : {}   voice={}",
                    who_detail(t),
                    t.cluster.as_deref().unwrap_or("—")
                ),
                format!(
                    "  conf     : {conf}   loudness: {loud} ({})",
                    clarity(t.loudness)
                ),
                format!("  model    : {}", t.model.as_deref().unwrap_or("—")),
                format!("  tier     : {}", t.tier),
                format!("  status   : {status}"),
                format!("  text     : {}", t.text),
            ]
            .join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// One session or day as a speaker-attributed transcript — what a reviewer
/// reads. Consecutive same-speaker turns are NOT merged here; the CLI's job is
/// to show each turn with its own timestamp, which is what makes a line
/// findable in the audio.
#[must_use]
pub fn transcript(title: &str, turns: &[Turn]) -> String {
    let mut header = format!("# {title}");
    if let Some(first) = turns.first() {
        header.push_str(&when(&first.start, "  (%a %d %b %Y %H:%M)"));
    }
    let mut lines = vec![header, String::new()];
    lines.extend(
        turns
            .iter()
            .map(|t| format!("[{}] {}: {}", when(&t.start, "%H:%M:%S"), who(t), t.text)),
    );
    lines.join("\n")
}
