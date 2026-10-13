//! The registrar: a microphone clip gets the `audio_segments` row its lines
//! hang from, with the duration decoded from the file. Uploads register
//! themselves (`upload::register`).

use crate::ledger::{Outcome, PassKind, record};
use audiocore::instant;
use audiocore::instant::Stamp;
use chrono::Duration;

crate::statements! {
    CAPTURE_SOURCES: Meaning =
        "SELECT id FROM sources WHERE kind NOT IN ('upload', ?1)";
    UNWRITTEN: Ingest =
        "SELECT s.filename, s.source FROM segments s
             WHERE s.source != ?1
               AND NOT EXISTS (SELECT 1 FROM pass_ledger l
                               WHERE l.kind = ?2 AND l.filename = s.filename)
             ORDER BY s.start_utc DESC";
    ADD_AUDIO_SEGMENT: Meaning =
        "INSERT OR IGNORE INTO audio_segments
                 (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
    AUDIO_PATHS: Meaning =
        "SELECT path FROM audio_segments";
    AUDIO_STARTS: Meaning =
        "SELECT source_id, start_utc FROM audio_segments";
}

/// What one registrar pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Registered {
    /// Clips given an `audio_segments` row.
    pub added: usize,
    /// Clips whose source the meaning plane does not know yet; the only
    /// outcome not ledgered.
    pub waiting: usize,
    /// Unparseable name, or ffmpeg could not read the file.
    pub unreadable: usize,
    /// Clips whose minute another copy already holds (`UNIQUE (source_id,
    /// start_utc)`): common, counted apart so the duplicates stay visible.
    pub covered: usize,
    /// Already registered by name.
    pub known: usize,
    /// Files decoded: the pass's whole cost.
    pub probed: usize,
}

/// Register microphone clips in the meaning plane, so their lines have audio.
///
/// - `end_utc` is decoded, not assumed: the view decides which live turns a
///   clip covers by it.
/// - An unknown source waits: its kind is the sender's to declare.
/// - `limit` bounds decodes only; cheap decisions drain in one pass.
/// - Every other outcome is ledgered, or it is decided again every pass.
///
/// # Errors
/// If either database refuses.
pub fn register_segments(
    meaning: &rusqlite::Connection,
    ingest: &rusqlite::Connection,
    root: &std::path::Path,
    now: &Stamp,
    limit: usize,
) -> rusqlite::Result<Registered> {
    // Not uploads, which register themselves, nor the room's blocks.
    let mics: std::collections::HashSet<String> = {
        let mut stmt = CAPTURE_SOURCES.prepare(meaning)?;
        let rows = stmt.query_map([crate::store::ROOM_KIND], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    let have = registered_names(meaning)?;
    let mut minutes = registered_minutes(meaning)?;

    // Newest first: a clip arriving now must not wait behind a backfill.
    let candidates: Vec<(String, String)> = {
        let mut stmt = UNWRITTEN.prepare(ingest)?;
        let rows = stmt.query_map((crate::store::ROOM_SOURCE, PassKind::Register), |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect::<Result<_, _>>()?
    };

    let mut out = Registered::default();
    let mut probes = 0;
    for (filename, source) in candidates {
        if probes >= limit {
            break;
        }
        let decided = |outcome| record(ingest, PassKind::Register, &filename, outcome, now);
        if have.contains(&filename) {
            decided(Outcome::AlreadyRegistered)?;
            out.known += 1;
            continue;
        }
        // Not ledgered: the source may be registered later.
        if !mics.contains(&source) {
            out.waiting += 1;
            continue;
        }
        let Some(start) = audiocore::names::parse_segment_start(&filename) else {
            out.unreadable += 1;
            decided(Outcome::Unnameable)?;
            continue;
        };
        // A sibling holds this minute: decided without decoding.
        let minute = (source.clone(), instant::python_isoformat_utc(start));
        if minutes.contains(&minute) {
            out.covered += 1;
            decided(Outcome::CoveredBySibling)?;
            continue;
        }
        let path = crate::store::source_dir(root, &source).join(&filename);
        probes += 1;
        out.probed += 1;
        let Ok(media) = crate::upload::probe(&path) else {
            // Permanent, e.g. a header-only file from a dead capture.
            out.unreadable += 1;
            decided(Outcome::Unreadable)?;
            continue;
        };
        let end = start + Duration::microseconds((media.duration_s * 1e6).round() as i64);
        let inserted = ADD_AUDIO_SEGMENT.execute(
            meaning,
            rusqlite::params![
                source,
                path.to_string_lossy(),
                instant::python_isoformat_utc(start),
                instant::python_isoformat_utc(end),
                media.sample_rate,
                media.channels,
            ],
        )?;
        // An ignored insert: a sibling holds this minute.
        if inserted == 1 {
            out.added += 1;
            // So a sibling later in this pass is not decoded.
            minutes.insert(minute);
        } else {
            out.covered += 1;
        }
        let outcome = if inserted == 1 {
            Outcome::Registered
        } else {
            Outcome::CoveredBySibling
        };
        decided(outcome)?;
    }
    Ok(out)
}

/// Every registered clip's basename, read once: a lookup per candidate takes
/// minutes.
fn registered_names(
    meaning: &rusqlite::Connection,
) -> rusqlite::Result<std::collections::HashSet<String>> {
    let mut stmt = AUDIO_PATHS.prepare(meaning)?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut set = std::collections::HashSet::new();
    for path in rows {
        if let Some(name) = path?.rsplit('/').next() {
            set.insert(name.to_owned());
        }
    }
    Ok(set)
}

/// Every registered `(source_id, start_utc)`, the `UNIQUE` key: catches
/// another copy of a minute before a decode.
fn registered_minutes(
    meaning: &rusqlite::Connection,
) -> rusqlite::Result<std::collections::HashSet<(String, String)>> {
    let mut stmt = AUDIO_STARTS.prepare(meaning)?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    rows.collect()
}
