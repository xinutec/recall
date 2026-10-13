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
    /// The shown words under a span of a clip.
    WORDS_UNDER: Meaning =
        "SELECT text, language FROM rendered_lines
         WHERE clip_id = ?1 AND hidden_reason IS NULL AND start_utc < ?3 AND end_utc > ?2
         ORDER BY start_utc, id";
    SPEAKER_NAMES: Meaning =
        "SELECT name FROM speakers \
         UNION \
         SELECT DISTINCT speaker_label FROM rendered_lines \
         WHERE speaker_label IS NOT NULL AND speaker_label NOT LIKE 'SPEAKER%' \
         ORDER BY name COLLATE NOCASE";
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

/// A person's naming of a stretch that teaches a voiceprint: in force, the
/// latest over its span, and not judged unusable. Newest first.
pub struct Labelled {
    pub id: transcript::EditId,
    pub clip: transcript::ClipId,
    pub span: transcript::Span,
    pub name: transcript::Name,
}

fn store_error(err: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(err))
}

/// Every labelled fragment.
pub fn labelled(conn: &Connection) -> rusqlite::Result<Vec<Labelled>> {
    use transcript::Act;
    let edits = crate::edit_log::all(conn).map_err(store_error)?;
    let gone = transcript::taken_back(&edits);
    let mut latest: std::collections::BTreeMap<(i64, transcript::Span), (Labelled, bool)> =
        std::collections::BTreeMap::new();
    for edit in edits.into_iter().filter(|e| !gone.contains(&e.id)) {
        if let Act::Speaker {
            clip,
            span,
            name,
            enrol,
        } = edit.act
        {
            let fragment = Labelled {
                id: edit.id,
                clip,
                span,
                name,
            };
            latest.insert((clip.stored(), span), (fragment, enrol));
        }
    }
    let mut out: Vec<Labelled> = latest
        .into_values()
        .filter(|(_, enrol)| *enrol)
        .map(|(l, _)| l)
        .collect();
    out.sort_by_key(|l| std::cmp::Reverse(l.id));
    Ok(out)
}

/// The labelled fragments, newest first, optionally of one voice.
pub fn list_corrections(
    conn: &Connection,
    speaker: Option<&str>,
    limit: i64,
) -> rusqlite::Result<Vec<Label>> {
    let spelled = |at: transcript::Instant| audiocore::instant::python_isoformat_utc(at.to_utc());
    labelled(conn)?
        .into_iter()
        .filter(|l| speaker.is_none_or(|who| l.name.as_str() == who))
        .take(usize::try_from(limit).unwrap_or(0))
        .map(|l| {
            let mut words = Vec::new();
            let mut language = None;
            let mut stmt = WORDS_UNDER.prepare(conn)?;
            let rows = stmt.query_map(
                rusqlite::params![
                    l.clip.stored(),
                    spelled(l.span.start()),
                    spelled(l.span.end())
                ],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
            )?;
            for row in rows {
                let (text, lang) = row?;
                words.push(text);
                language = language.or(lang);
            }
            let id = l.id.stored();
            Ok(Label {
                id,
                text: words.join(" "),
                speaker: Some(l.name.as_str().to_owned()),
                language,
                start: spelled(l.span.start()),
                audio_url: format!("/api/correction/{id}/audio"),
            })
        })
        .collect()
}

/// How many labels each voice has.
pub fn corrections_by_speaker(
    conn: &Connection,
) -> rusqlite::Result<std::collections::BTreeMap<String, i64>> {
    let mut counts = std::collections::BTreeMap::new();
    for l in labelled(conn)? {
        *counts.entry(l.name.as_str().to_owned()).or_default() += 1;
    }
    Ok(counts)
}

/// A labelled fragment's audio file and the span it names, in seconds into
/// the file.
pub fn correction_placement(
    conn: &Connection,
    ingest: &Connection,
    root: &std::path::Path,
    id: i64,
) -> rusqlite::Result<Option<(std::path::PathBuf, f64, f64)>> {
    let Some(l) = labelled(conn)?.into_iter().find(|l| l.id.stored() == id) else {
        return Ok(None);
    };
    let Some(clip) = crate::clips::by_id(ingest, l.clip).map_err(store_error)? else {
        return Ok(None);
    };
    let at = |i: transcript::Instant| (i.micros() - clip.start.micros()) as f64 / 1e6;
    Ok(Some((
        root.join(&clip.path),
        at(l.span.start()),
        at(l.span.end()),
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
/// prompt name (about 1% of clips under two seconds), but on short fragments
/// names were spelled right about 4x as often with it (few samples).
/// `quality::is_bare_name` refuses a live turn that is only a name. On whole
/// minutes, as the archive pass decodes them, it buys little: of 54 glossary
/// words in people's corrections it found 17, against 14 without and 15 with
/// the words as a sentence. Re-measure with the runner's `prompt_cost` and
/// `prompt_names` examples.
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
        let ingest_root = root.clone();
        audio::render_blocking(&root, false, move |conn| {
            let ingest = crate::store::open(&ingest_root)?;
            Ok(correction_placement(conn, &ingest, &ingest_root, id)?.map(
                |(path, start_s, end_s)| {
                    let (start, end) = correction_window(start_s, end_s, q.context);
                    (path, start, end)
                },
            ))
        })
    });
    match rendered.await {
        Ok(response) => response,
        Err(err) => route::faulted("correction audio task", &err),
    }
}
