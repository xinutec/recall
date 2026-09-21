//! Does every segment that ever had turns still show at least one?
//!
//! A segment with only hidden turns and no visible replacement is an impossible
//! state under refine's contract ("a refine replaces a transcript or keeps it,
//! never empties one") — yet 175 segments sat that way for weeks in July until a
//! human noticed a minute of Dutch missing. This is the day-one detector for
//! that class: every count here is a stretch of household memory currently
//! invisible, and `recall repair` puts the newest hidden generation back.
//!
//! ⚠ **The detector gates it.** Not every provenance hide was a bug: sometimes a
//! later pass was correctly dropping a hallucination, and restoring that
//! resurrects garbage. On this archive 12 of 170 restorations did exactly that —
//! "E aí", "т т т т", repeated glyphs on -64 dB silence — and they then blocked
//! the cleanup by making an empty minute look transcribed. A segment the VAD
//! heard nothing in gets nothing back, and a generation that is entirely junk is
//! not counted as restorable.

// ⚠ Shared, never copied here. recalld judges the same text for the OPPOSITE
// purpose — it decides whether to write a turn, this decides whether a hidden
// one is worth restoring — so a drifted copy reports household memory as
// unrecoverable. A copy drifted exactly that way once.
use audiocore::text::{is_repetition_loop, is_wordless};

/// Reasons a turn was hidden on the EVIDENCE of what it was, rather than by a
/// pass replacing it. Such a turn is never a candidate for restoring.
///
/// ⚠ These are the sentences `recall.cleanup` writes, verbatim — not slugs.
/// Guessed slug spellings matched nothing, and a reason that never matches
/// makes every evidence-hidden turn look like a restorable generation: against
/// the real archive that reported 11 blanked segments where there are none.
const EVIDENCE_REASONS: [&str; 4] = [
    "no speech detected (VAD)",
    "repetition loop",
    "non-Latin script, no speech (VAD)",
    "no words",
];

/// One hidden turn, as `segments_showing_no_turns` returns it.
#[derive(Debug, Clone)]
pub struct HiddenTurn {
    pub id: i64,
    pub hidden_reason: String,
    pub text: String,
}

/// The newest generation: the trailing run, in id order, hidden by a single
/// pass.
///
/// Each pass hides the generation before it, so a segment accretes them —
/// original, reprocessed, diarized — and the *last* run sharing one hidden
/// reason is the newest, and best, transcript that ever existed for it. A turn
/// hidden on the evidence of what it was (a hallucination, a repetition loop) is
/// never restored.
pub fn last_generation(turns: &[HiddenTurn]) -> Vec<i64> {
    let generations: Vec<&HiddenTurn> = turns
        .iter()
        .filter(|t| !EVIDENCE_REASONS.contains(&t.hidden_reason.as_str()))
        .collect();
    let Some(newest) = generations.last() else {
        return Vec::new();
    };
    let newest_reason = newest.hidden_reason.as_str();
    let mut run: Vec<i64> = generations
        .iter()
        .rev()
        .take_while(|t| t.hidden_reason == newest_reason)
        .map(|t| t.id)
        .collect();
    run.reverse();
    run
}

/// The subset worth bringing back: whatever cleanup would not hide on sight.
///
/// Pure text only — the foreign-script rule needs audio, and the caller has
/// already excluded segments the detector heard nothing in, so script over real
/// speech is protected there rather than here.
pub fn any_restorable(texts: &[&str]) -> bool {
    texts
        .iter()
        .any(|t| !is_wordless(t) && !is_repetition_loop(t))
}
