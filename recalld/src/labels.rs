//! The labelling surface's reads: the speaker roster, the labelled fragments,
//! and one fragment's audio. The writes are in `labels_write`.

use crate::audio::{self, clip_window};
use crate::reads;
use crate::route;
use axum::extract::{Query, State};
use axum::response::Response;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

crate::statements! {
    CORRECTIONS_BY: Meaning =
        "SELECT id, start_utc, corrected_text, speaker, language FROM corrections \
         WHERE hidden_reason IS NULL AND speaker = ?1 ORDER BY id DESC LIMIT ?2";
    CORRECTIONS: Meaning =
        "SELECT id, start_utc, corrected_text, speaker, language FROM corrections \
         WHERE hidden_reason IS NULL ORDER BY id DESC LIMIT ?1";
    SPEAKER_NAMES: Meaning =
        "SELECT name FROM speakers \
         UNION \
         SELECT DISTINCT speaker_label FROM transcript_segments \
         WHERE speaker_label IS NOT NULL AND speaker_label NOT LIKE 'SPEAKER%' \
         ORDER BY name COLLATE NOCASE";
    LABEL_COUNTS: Meaning =
        "SELECT COALESCE(speaker, ''), COUNT(*) FROM corrections \
         WHERE hidden_reason IS NULL GROUP BY 1";
    CORRECTION_AUDIO: Meaning =
        "SELECT a.path, \
                (julianday(c.start_utc) - julianday(a.start_utc)) * 86400.0, \
                (julianday(c.end_utc)   - julianday(a.start_utc)) * 86400.0 \
         FROM corrections c \
         JOIN audio_segments a ON a.id = c.audio_segment_id \
         WHERE c.id = ?1 AND c.audio_segment_id IS NOT NULL";
}

/// Padding and minimum length when a fragment is played with context.
const PAD_S: f64 = 1.5;
const MIN_S: f64 = 5.0;

#[derive(Debug, Serialize, PartialEq, Eq, ts_rs::TS)]
#[ts(export)]
pub struct SpeakerNames {
    pub names: Vec<String>,
}

/// Every name in use: enrolled voices and human labels. Cluster tags
/// (`SPEAKER_00`) are not people, so they are not offered.
pub fn known_speaker_names(conn: &Connection) -> rusqlite::Result<SpeakerNames> {
    let mut stmt = SPEAKER_NAMES.prepare(conn)?;
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

#[derive(Debug, Serialize, PartialEq, Eq, ts_rs::TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Label {
    pub id: i64,
    pub text: String,
    pub speaker: Option<String>,
    pub language: Option<String>,
    pub start: String,
    pub audio_url: String,
}

#[derive(Debug, Serialize, PartialEq, Eq, ts_rs::TS)]
#[ts(export, rename = "LabelList")]
#[serde(rename_all = "camelCase")]
pub struct CorrectionsOut {
    pub items: Vec<Label>,
    #[ts(type = "Record<string, number>")]
    pub by_speaker: std::collections::BTreeMap<String, i64>,
}

/// The labelled fragments, newest first, optionally of one voice. Hidden
/// corrections (kept out of enrolment) are excluded.
pub fn list_corrections(
    conn: &Connection,
    speaker: Option<&str>,
    limit: i64,
) -> rusqlite::Result<Vec<Label>> {
    let sql = if speaker.is_some() {
        CORRECTIONS_BY
    } else {
        CORRECTIONS
    };
    let mut stmt = sql.prepare(conn)?;
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

/// How many labels each voice has.
pub fn corrections_by_speaker(
    conn: &Connection,
) -> rusqlite::Result<std::collections::BTreeMap<String, i64>> {
    let mut stmt = LABEL_COUNTS.prepare(conn)?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    rows.collect()
}

/// A correction's audio file and the span it was cut from.
pub fn correction_placement(
    conn: &Connection,
    correction_id: i64,
) -> rusqlite::Result<Option<(std::path::PathBuf, f64, f64)>> {
    let mut stmt = CORRECTION_AUDIO.prepare(conn)?;
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

/// Exact unless `context`: the Labels page audits the cut, and padding would
/// hide a wrong span.
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

/// Whisper reserves 224 tokens for the prompt; stay well under it.
const MAX_PROMPT_CHARS: usize = 600;

/// The household glossary as Whisper's `initial_prompt`: speaker names, then
/// the vocabulary, cut at the length cap.
///
/// Kept for short clips too. On audio it cannot place, the model may emit a
/// prompt name (about 1% of clips under two seconds), but names are spelled
/// right about 4x as often with it (few samples). `quality::is_bare_name`
/// refuses a live turn that is only a name. Re-measure with the runner's
/// `prompt_cost` and `prompt_spelling` examples.
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
    let rendered = tokio::task::spawn_blocking(move || {
        // No enhancement: labelling judges the recording as it is.
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
