//! The port against the REAL archive — the evidence the synthetic fixture cannot
//! give.
//!
//! `identify_parity` proves agreement on generated vectors. What it cannot show
//! is agreement on this household's actual voices, because the repository is
//! public and the voiceprints are people. So this reads the live archive when it
//! is present and reports RATES ONLY: no name, no vector, no text ever leaves it.
//!
//! ⚠ **This settles the MATCHER, not the embedding.** It compares Rust and
//! Python over the SAME stored vectors, so it answers "did the arithmetic port
//! correctly". Whether a per-speaker embedding attributes as well as `refine`'s
//! per-turn one is a different question needing re-embedded audio, and this says
//! nothing about it.
//!
//! ⚠ **ITS BASELINE IS ERODING, and nothing here will say so.** The stored
//! `speaker_guess` values this compares against were written by Python, which was
//! deleted on 2026-09-17 — and `recalld::diarized` now writes guesses of its own
//! over the same rows. Every turn the Rust re-derives makes this compare Rust to
//! Rust and agree trivially. The 1.000000 it last reported over 28,153 turns was
//! a real result; a future one may be an empty tautology, so read the DATE of the
//! rows it scored, not only the rate.
//!
//! Ignored by default: it depends on a machine-specific file and would fail
//! everywhere else. Run it deliberately:
//!
//! ```text
//! cargo test -p recalld --test integration identify_differential -- --ignored --nocapture
//! ```

use recalld::identify::{Voiceprint, match_one};
use rusqlite::Connection;

const ARCHIVE: &str = "/Volumes/Backup/recall/recall.sqlite";

/// Agreement must be total. These are not two heuristics being compared — they
/// are one rule in two languages, over identical inputs, so any disagreement is a
/// porting bug rather than a tuning difference.
const REQUIRED_AGREEMENT: f64 = 1.0;

#[test]
#[ignore = "reads the live archive; run deliberately with --ignored"]
fn the_port_agrees_with_the_python_on_the_real_archive() {
    let Ok(conn) = Connection::open_with_flags(
        ARCHIVE,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ) else {
        panic!("no archive at {ARCHIVE} — run this where it is mounted");
    };

    let enrolled: Vec<Voiceprint> = recalld::identify::enrolled(&conn).expect("voiceprints");
    assert!(
        enrolled.len() > 10,
        "only {} voiceprints — too few to be the real archive",
        enrolled.len()
    );

    let mut stmt = conn
        .prepare(
            // ⚠ **THE SAME POPULATION `rematch_speaker_guesses` MAINTAINS**, and
            // getting this wrong is how the first run of this test reported a 4%
            // porting bug that did not exist. Without these three filters the
            // query also returns superseded, hidden and human-labelled turns —
            // rows whose cached guess Python deliberately never refreshes, so
            // they are stale against ANY correct matcher. Comparing against them
            // measures the staleness you selected for.
            "SELECT te.vector, ts.speaker_guess, ts.speaker_score
             FROM transcript_embeddings te
             JOIN transcript_segments ts ON ts.id = te.segment_id
             WHERE ts.superseded_by IS NULL AND ts.hidden_reason IS NULL
               AND ts.speaker_label IS NULL
               AND ts.speaker_guess IS NOT NULL AND ts.speaker_score IS NOT NULL",
        )
        .expect("prepare");
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
            ))
        })
        .expect("query");

    let (mut compared, mut same_name, mut same_score, mut unreadable) = (0u64, 0u64, 0u64, 0u64);
    let mut worst_score_gap = 0.0f64;
    for row in rows {
        let (raw, py_name, py_score) = row.expect("row");
        let Ok(vector) = serde_json::from_str::<Vec<f64>>(&raw) else {
            unreadable += 1;
            continue;
        };
        let Some(got) = match_one(&vector, &enrolled) else {
            unreadable += 1;
            continue;
        };
        compared += 1;
        if got.person == py_name {
            same_name += 1;
        }
        let gap = (got.score - py_score).abs();
        worst_score_gap = worst_score_gap.max(gap);
        // ⚠ 1e-4, because that is the Python's OWN write threshold
        // (`_SCORE_EPSILON`): it leaves a stored score alone below that, so the
        // cache is allowed to lag by up to it. Demanding 1e-6 against a stored
        // value asks for more agreement than the writer intends.
        if gap <= 1e-4 {
            same_score += 1;
        }
    }

    assert!(compared > 1000, "only {compared} turns compared");
    #[expect(clippy::cast_precision_loss, reason = "counts, far inside f64")]
    let name_rate = same_name as f64 / compared as f64;
    #[expect(clippy::cast_precision_loss, reason = "counts, far inside f64")]
    let score_rate = same_score as f64 / compared as f64;
    println!(
        "compared={compared} unreadable={unreadable} \
         name_agreement={name_rate:.6} score_agreement={score_rate:.6} \
         worst_score_gap={worst_score_gap:.3e}"
    );

    assert!(
        name_rate >= REQUIRED_AGREEMENT,
        "name agreement {name_rate:.6} over {compared} turns — a porting bug, \
         not a tuning difference"
    );
    assert!(
        score_rate >= REQUIRED_AGREEMENT,
        "score agreement {score_rate:.6}, worst gap {worst_score_gap:.3e}"
    );
}
