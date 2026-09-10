//! The labelling surface's READ half (stage F1), from `recall.api_labels`:
//! the speaker roster, the labelled fragments, and one fragment's audio.
//!
//! ⚠ **Only the reads. The writes are deliberately NOT here yet**, and the reason
//! is what they touch: `/api/correct`, the speaker assignments and the hide are
//! the human half of the system of record — 468 corrections that no pass can
//! re-derive, since a person listened and typed them (docs/architecture.md,
//! "What must survive"). A correction is applied by superseding a turn and
//! recording the pair, so a subtly wrong port does not fail loudly; it writes a
//! wrong chain into the one table that cannot be rebuilt. Those move in their own
//! change, with the supersede chain pinned by tests, rather than riding along
//! with three read routes.
//!
//! ⚠ **`/api/suggest` and `/api/sessions/{s}/voices` are also held back**, for a
//! different reason: they are the voiceprint SUGGESTION surface, and whether that
//! stays in the product is an open question for Pippijn (the timeline's auto-guess
//! chip is the same family). Porting a feature that may be cut is work spent
//! twice.

use crate::audio::{self, clip_window};
use crate::reads;
use crate::route;
use axum::extract::{Query, State};
use axum::response::Response;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Lead-in/-out when a fragment is played WITH context.
const PAD_S: f64 = 1.5;
/// Minimum length for a context clip, so a short fragment is listenable.
const MIN_S: f64 = 5.0;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SpeakerNames {
    pub names: Vec<String>,
}

/// Every name already in use — enrolled voices plus human-assigned labels.
///
/// ⚠ The `SPEAKER%` exclusion is load-bearing: diarization's raw cluster tags
/// (`SPEAKER_00`) are not people, and offering them as autocomplete would spread
/// a machine tag into the roster one accepted suggestion at a time.
pub fn known_speaker_names(conn: &Connection) -> rusqlite::Result<SpeakerNames> {
    let mut stmt = conn.prepare(
        "SELECT name FROM speakers \
         UNION \
         SELECT DISTINCT speaker_label FROM transcript_segments \
         WHERE speaker_label IS NOT NULL AND speaker_label NOT LIKE 'SPEAKER%' \
         ORDER BY name COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, Option<String>>(0))?;
    let mut names = Vec::new();
    for row in rows {
        if let Some(name) = row?
            && !name.is_empty()
        {
            names.push(name);
        }
    }
    Ok(SpeakerNames { names })
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Label {
    pub id: i64,
    pub text: String,
    pub speaker: Option<String>,
    pub language: Option<String>,
    pub start: String,
    pub audio_url: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CorrectionsOut {
    pub items: Vec<Label>,
    pub by_speaker: std::collections::BTreeMap<String, i64>,
}

/// The labelled fragments for review, newest first, optionally one voice.
///
/// ⚠ `hidden_reason IS NULL`: a correction hidden as mistaken must not come back
/// into the review list, because the whole point of hiding one is that it was
/// poisoning enrolment.
pub fn list_corrections(
    conn: &Connection,
    speaker: Option<&str>,
    limit: i64,
) -> rusqlite::Result<Vec<Label>> {
    let sql = if speaker.is_some() {
        "SELECT id, start_utc, corrected_text, speaker, language FROM corrections \
         WHERE hidden_reason IS NULL AND speaker = ?1 ORDER BY id DESC LIMIT ?2"
    } else {
        "SELECT id, start_utc, corrected_text, speaker, language FROM corrections \
         WHERE hidden_reason IS NULL ORDER BY id DESC LIMIT ?1"
    };
    let mut stmt = conn.prepare(sql)?;
    let to_label = |r: &rusqlite::Row| -> rusqlite::Result<Label> {
        let id: i64 = r.get(0)?;
        Ok(Label {
            id,
            start: r.get(1)?,
            text: r.get(2)?,
            speaker: r.get(3)?,
            language: r.get(4)?,
            audio_url: format!("/api/correction/{id}/audio"),
        })
    };
    let rows = match speaker {
        Some(who) => stmt.query_map(rusqlite::params![who, limit], to_label)?,
        None => stmt.query_map(rusqlite::params![limit], to_label)?,
    };
    rows.collect()
}

/// How many labels each voice has — the progress strip on the Labels page.
pub fn corrections_by_speaker(
    conn: &Connection,
) -> rusqlite::Result<std::collections::BTreeMap<String, i64>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(speaker, ''), COUNT(*) FROM corrections \
         WHERE hidden_reason IS NULL GROUP BY 1",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    rows.collect()
}

/// Where a correction's audio lives, and the exact span it was cut from.
pub fn correction_placement(
    conn: &Connection,
    correction_id: i64,
) -> rusqlite::Result<Option<(std::path::PathBuf, f64, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT a.path, \
                (julianday(c.start_utc) - julianday(a.start_utc)) * 86400.0, \
                (julianday(c.end_utc)   - julianday(a.start_utc)) * 86400.0 \
         FROM corrections c \
         JOIN audio_segments a ON a.id = c.audio_segment_id \
         WHERE c.id = ?1 AND c.audio_segment_id IS NOT NULL",
    )?;
    let mut rows = stmt.query([correction_id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some((
        std::path::PathBuf::from(row.get::<_, String>(0)?),
        row.get(1)?,
        row.get(2)?,
    )))
}

/// The window for a labelled clip.
///
/// ⚠ Exact by DEFAULT, padded only on request — the inverse of a turn's playback,
/// and deliberately so. The Labels page exists to AUDIT the cut: if the span is
/// wrong, padding it hides the very defect you opened the page to see. `context`
/// is for when you cannot recognise a voice from the trimmed fragment.
#[must_use]
pub fn correction_window(start_s: f64, end_s: f64, context: bool) -> (f64, f64) {
    if context {
        clip_window(start_s, end_s, PAD_S, MIN_S)
    } else {
        (start_s.max(0.0), end_s)
    }
}

// --- HTTP ------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CorrectionsQuery {
    speaker: Option<String>,
    #[serde(default = "default_limit")]
    limit: i64,
}

const fn default_limit() -> i64 {
    200
}

#[derive(Deserialize)]
pub struct AudioQuery {
    #[serde(default)]
    context: bool,
}

/// Whisper reserves 224 tokens for the prompt; stay comfortably under it so the
/// bias list can never crowd out real left-context.
const MAX_PROMPT_CHARS: usize = 600;

/// One human naming of a voice, as the fleet→Mac label channel carries it.
///
/// ⚠ The field names are `snake_case` ON THE WIRE — no camelCase rename. The
/// Python model declares them bare and the Mac's client parses them bare, so
/// tidying this into `sourceId` would silently drop every label at the Mac.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ClusterNaming {
    pub source_id: String,
    pub cluster: String,
    pub name: String,
}

/// Every human naming of a voice — the whole set, so a missed pass self-heals
/// and the Mac has a diff baseline.
///
/// ⚠ A cluster is ONE voice, so when a few of its turns were individually
/// reassigned the cluster's DOMINANT (most-turns) label wins. Ties break on the
/// label text, and the row order is the payload's order — both are load-bearing
/// for a stable payload, not incidental.
pub fn cluster_namings(conn: &Connection) -> rusqlite::Result<Vec<ClusterNaming>> {
    let mut stmt = conn.prepare(
        "SELECT a.source_id src, ts.speaker_cluster cl, ts.speaker_label lbl, COUNT(*) n \
         FROM transcript_segments ts \
         JOIN audio_segments a ON a.id = ts.audio_segment_id \
         WHERE ts.speaker_label IS NOT NULL AND ts.speaker_cluster IS NOT NULL \
           AND ts.superseded_by IS NULL AND ts.hidden_reason IS NULL \
         GROUP BY a.source_id, ts.speaker_cluster, ts.speaker_label \
         ORDER BY a.source_id, ts.speaker_cluster, n DESC, ts.speaker_label",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    // The FIRST row per (source, cluster) is its highest-count label; later rows
    // for the same pair are the minority relabels and are dropped.
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        let (source_id, cluster, name) = row?;
        if seen.insert((source_id.clone(), cluster.clone())) {
            out.push(ClusterNaming {
                source_id,
                cluster,
                name,
            });
        }
    }
    Ok(out)
}

/// The household glossary as Whisper's `initial_prompt`, or `None` when empty.
///
/// Enrolled speaker names first (short, highest value), then the explicit
/// vocabulary, as a plain comma list — Whisper only needs to SEE the tokens.
///
/// ⚠ The length rule BREAKS, it does not skip. Once one term would take the
/// prompt past the cap the list ends there, so the result is always a prefix.
/// Skipping the long one and carrying on would silently reorder what the model
/// is biased toward, and would make the prompt depend on which terms happen to
/// be long rather than on their priority.
pub fn initial_prompt(conn: &Connection) -> rusqlite::Result<Option<String>> {
    let mut ordered: Vec<String> = known_speaker_names(conn)?.names;
    ordered.extend(
        crate::work::vocabulary(conn)?
            .items
            .into_iter()
            .map(|t| t.term),
    );

    let mut seen = std::collections::HashSet::new();
    let mut prompt = String::new();
    for term in ordered {
        if !seen.insert(term.clone()) {
            continue;
        }
        let extended = if prompt.is_empty() {
            term
        } else {
            format!("{prompt}, {term}")
        };
        if extended.chars().count() > MAX_PROMPT_CHARS {
            break;
        }
        prompt = extended;
    }
    Ok(if prompt.is_empty() {
        None
    } else {
        Some(prompt)
    })
}

pub async fn speakers_route(State(st): State<Arc<reads::State>>) -> Response {
    let root = st.root.clone();
    route::json("speakers", move || {
        known_speaker_names(&reads::open(&root)?)
    })
    .await
}

pub async fn corrections_route(
    State(st): State<Arc<reads::State>>,
    Query(q): Query<CorrectionsQuery>,
) -> Response {
    let root = st.root.clone();
    let limit = q.limit.clamp(0, 1000);
    route::json("corrections", move || {
        let conn = reads::open(&root)?;
        Ok(CorrectionsOut {
            items: list_corrections(&conn, q.speaker.as_deref(), limit)?,
            by_speaker: corrections_by_speaker(&conn)?,
        })
    })
    .await
}

pub async fn correction_audio_route(
    State(st): State<Arc<reads::State>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Query(q): Query<AudioQuery>,
) -> Response {
    let root = st.root.clone();
    // ⚠ The render must happen INSIDE the blocking task, not after awaiting it:
    // it shells out to ffmpeg, and on the runtime thread that stalls every other
    // request for the length of the clip. Going through `render_blocking` also
    // makes this behave exactly like the other two clip routes.
    let rendered = tokio::task::spawn_blocking(move || {
        // Labelling wants the recording as it is — fidelity is the thing being
        // judged — so no enhance option here.
        audio::render_blocking(&root, false, |conn| {
            Ok(
                correction_placement(conn, id)?.map(|(path, start_s, end_s)| {
                    let (start, end) = correction_window(start_s, end_s, q.context);
                    (path, start, end)
                }),
            )
        })
    });
    match rendered.await {
        Ok(response) => response,
        Err(err) => route::faulted("correction audio task", &err),
    }
}
