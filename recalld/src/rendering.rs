//! The bridge from storage to [`transcript::render`] (#1911): gather one clip's
//! inputs — its current model results, the speech detector's evidence, the
//! edit log and the voiceprints — and render it.
//!
//! Only conversion lives here; every rule is in `render`.

use crate::results::{self, Outcome};
use audiocore::shim::{asr, voices};
use rusqlite::Connection;
use transcript::render::{
    self, Heard, HeardSegment, HeardWord, Rendered, Speech, VoiceTurn, Voices,
};
use transcript::voice::Voiceprint;
use transcript::{Clip, Edit};

/// What a clip's lines are made from, apart from the edit log and the prints,
/// which are shared by every clip and loaded once.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub heard: Option<Heard>,
    pub voices: Option<Voices>,
    pub speech: Speech,
}

/// Read the clip's current results and speech evidence.
pub fn facts(ingest: &Connection, clip: &Clip) -> Result<Facts, results::ResultError> {
    let heard = match results::transcription(ingest, clip)? {
        Some(Outcome::Answer { answer, .. }) => Some(heard(&answer)),
        Some(Outcome::Refused { .. }) | None => None,
    };
    let segment_prints = match results::segment_prints(ingest, clip)? {
        Some(Outcome::Answer { answer, .. }) => answer
            .prints
            .into_iter()
            .filter_map(|p| Some((usize::try_from(p.segment_id).ok()?, p.vector)))
            .collect(),
        Some(Outcome::Refused { .. }) | None => Vec::new(),
    };
    let voices = match results::diarization(ingest, clip)? {
        Some(Outcome::Answer { answer, .. }) => Some(Voices {
            segment_prints,
            ..voices_of(&answer)
        }),
        Some(Outcome::Refused { .. }) | None => None,
    };
    Ok(Facts {
        heard,
        voices,
        speech: crate::speech::evidence(ingest, &clip.filename)?,
    })
}

/// The clip's lines.
pub fn lines(clip: &Clip, facts: &Facts, edits: &[Edit], enrolled: &[Voiceprint]) -> Rendered {
    render::render(&render::Input {
        clip,
        heard: facts.heard.as_ref(),
        voices: facts.voices.as_ref(),
        speech: &facts.speech,
        edits,
        enrolled,
    })
}

/// A stored segment without times cannot be placed, and is left out. Its
/// segments' indices are the ones [`Kind::EmbedLines`] prints are keyed by.
///
/// [`Kind::EmbedLines`]: audiocore::job::Kind::EmbedLines
pub fn heard(reply: &asr::Reply) -> Heard {
    Heard {
        language: reply.language.clone(),
        segments: reply
            .segments
            .iter()
            .filter_map(|s| {
                Some(HeardSegment {
                    start: s.start?,
                    end: s.end?,
                    text: s.text.clone(),
                    words: s.words.as_ref().map(|words| {
                        words
                            .iter()
                            .map(|w| HeardWord {
                                start: w.start,
                                end: w.end,
                                text: w.text.clone(),
                                probability: w.probability,
                            })
                            .collect()
                    }),
                })
            })
            .collect(),
    }
}

fn voices_of(diarization: &voices::Diarization) -> Voices {
    Voices {
        turns: diarization
            .turns
            .iter()
            .map(|t| VoiceTurn {
                speaker: t.speaker.clone(),
                start: t.start,
                end: t.end,
            })
            .collect(),
        prints: diarization
            .speakers
            .iter()
            .map(|s| (s.speaker.clone(), s.vector.clone()))
            .collect(),
        segment_prints: Vec::new(),
    }
}

/// A segment shorter than this cannot be embedded: the model's kernel is
/// longer than its frames.
const SHORTEST_EMBEDDABLE_S: f64 = 0.5;

/// Fill an [`Kind::EmbedLines`] job's spans at lease time: the clip's
/// transcribed segments long enough to embed, each by its index in [`heard`],
/// the index [`facts`] keys the prints by. Other kinds are left as they are.
///
/// [`Kind::EmbedLines`]: audiocore::job::Kind::EmbedLines
///
/// # Errors
/// If the ingest plane refuses.
pub fn attach_segment_spans(
    root: &std::path::Path,
    job: &mut audiocore::work::Job,
) -> rusqlite::Result<()> {
    if job.kind != audiocore::job::Kind::EmbedLines {
        return Ok(());
    }
    let reply = match results::transcription_of(&crate::store::open(root)?, &job.filename) {
        Ok(Some(Outcome::Answer { answer, .. })) => answer,
        Ok(_) => return Ok(()),
        Err(results::ResultError::Db(err)) => return Err(err),
        // Unreadable: no spans, and the job stores an empty result.
        Err(err) => {
            tracing::warn!(filename = %job.filename, %err, "embed-lines: transcription does not read");
            return Ok(());
        }
    };
    job.spans = heard(&reply)
        .segments
        .iter()
        .enumerate()
        .filter(|(_, s)| s.end - s.start >= SHORTEST_EMBEDDABLE_S)
        .map(|(i, s)| audiocore::work::Span {
            segment_id: i64::try_from(i).unwrap_or(i64::MAX),
            start_s: s.start,
            end_s: s.end,
        })
        .collect();
    Ok(())
}
