//! The matcher against the live archive's real voices, which a public repository
//! cannot hold as a fixture. It reports rates only: no name, vector or text.
//!
//! It compares the matcher over the same stored vectors, so it checks the
//! arithmetic, not whether a per-speaker embedding attributes as well as a
//! per-turn one.
//!
//! ⚠ The stored `speaker_guess` baseline comes from the Python matcher, but
//! `recalld::diarized` rewrites guesses on the same rows, and every rewritten row
//! agrees trivially. Read the age of the scored rows, not only the rate.
//!
//! Ignored by default because it needs a machine-specific file. Run it with:
//!
//! ```text
//! cargo test -p recalld --test integration identify_differential -- --ignored --nocapture
//! ```

use recalld::identify::{Voiceprint, match_one};
use rusqlite::Connection;

const ARCHIVE: &str = "/Volumes/Backup/recall/recall.sqlite";

/// Total: one rule in two languages over identical inputs, so any disagreement
/// is a porting bug, not a tuning difference.
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
            // ⚠ The same population `rematch::run_once` maintains.
            // Superseded, hidden and human-labelled turns never get their cached
            // guess refreshed, so they are stale against any correct matcher.
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
        // 1e-4 is the writer's own threshold (`_SCORE_EPSILON`): a stored score
        // is left alone below it, so the cache may lag by that much.
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
