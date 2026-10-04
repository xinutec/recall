//! The bridge from storage to [`transcript::render`] (#1911): gather one clip's
//! inputs — its current model results, the speech detector's evidence, the
//! edit log and the voiceprints — and render it.
//!
//! Only conversion lives here; every rule is in `render`.

use crate::results::{self, Outcome};
use audiocore::shim::{asr, voices};
use rusqlite::{Connection, OptionalExtension};
use transcript::render::{
    self, Heard, HeardSegment, HeardWord, Rendered, Speech, VoiceTurn, Voices,
};
use transcript::voice::Voiceprint;
use transcript::{Clip, Edit};

crate::statements! {
    SPEECH: Ingest = "SELECT speech_seconds, regions FROM segment_speech WHERE filename = ?1";
}

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
    let heard = match results::transcription(ingest, clip.id)? {
        Some(Outcome::Answer { answer, .. }) => Some(heard(&answer)),
        Some(Outcome::Refused { .. }) | None => None,
    };
    let voices = match results::diarization(ingest, clip.id)? {
        Some(Outcome::Answer { answer, .. }) => Some(voices_of(&answer)),
        Some(Outcome::Refused { .. }) | None => None,
    };
    Ok(Facts {
        heard,
        voices,
        speech: speech(ingest, &clip.filename)?,
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

/// A stored segment without times cannot be placed, and is left out.
fn heard(reply: &asr::Reply) -> Heard {
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
    }
}

/// The detector's evidence. Regions are stored as JSON `[[start, end], ...]`,
/// or JSON `null` when the clip could not be decoded; NULL when not yet looked.
fn speech(ingest: &Connection, filename: &str) -> rusqlite::Result<Speech> {
    let Some((seconds, regions)) = SPEECH
        .query_row(ingest, [filename], |r| {
            Ok((r.get::<_, f64>(0)?, r.get::<_, Option<String>>(1)?))
        })
        .optional()?
    else {
        return Ok(Speech::default());
    };
    let regions = regions.and_then(|raw| {
        serde_json::from_str::<Option<Vec<(f64, f64)>>>(&raw)
            .ok()
            .flatten()
    });
    Ok(Speech {
        seconds: (seconds >= 0.0).then_some(seconds),
        regions,
    })
}
