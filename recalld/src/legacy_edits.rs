//! The edit log, recovered from the tables that hold what people did today
//! (#1912): corrections, named lines, split pieces, hides and language pins.
//!
//! [`convert`] is pure: rows in, acts out, and every row accounted for, either
//! as the acts it became or as the reason it became none. [`load`] reads the
//! rows. Nothing is stored here: until writes switch to the log (#1913), the
//! old tables stay the record and this runs fresh each time it is asked.

use crate::clips::{self, ClipError};
use rusqlite::Connection;
use std::path::Path;
use transcript::{Act, ClipId, Instant, Language, Name, SourceId, Span, Text};

crate::statements! {
    CORRECTIONS: Meaning =
        "SELECT c.id, c.created_utc, a.path, c.start_utc, c.end_utc, c.original_text,
                c.corrected_text, c.speaker, c.words_checked, c.hidden_reason
         FROM corrections c JOIN audio_segments a ON a.id = c.audio_segment_id
         ORDER BY c.created_utc, c.id";
    /// Visible lines a person shaped without a correction row: named lines and
    /// the pieces of split ones. Lines a correction produced are its own rows.
    NAMED_LINES: Meaning =
        "SELECT t.id, t.created_utc, a.path, t.start_utc, t.end_utc, t.text, t.speaker_label,
                t.asr_model = 'human'
         FROM transcript_segments t JOIN audio_segments a ON a.id = t.audio_segment_id
         WHERE t.superseded_by IS NULL AND t.hidden_reason IS NULL
           AND (t.speaker_label IS NOT NULL OR t.asr_model = 'human')
           AND coalesce(t.provenance, '') NOT LIKE 'human correction of #%'
         ORDER BY t.created_utc, t.id";
    HIDES: Meaning =
        "SELECT t.id, t.created_utc, a.path, t.start_utc, t.end_utc, t.hidden_reason
         FROM transcript_segments t JOIN audio_segments a ON a.id = t.audio_segment_id
         WHERE t.superseded_by IS NULL AND t.hidden_reason IN (?1, ?2)
         ORDER BY t.created_utc, t.id";
    PINS: Meaning = "SELECT id, language FROM sources WHERE language IS NOT NULL ORDER BY id";
}

/// What `hidden_reason` says when a person marked a line.
pub const NOBODY_SPOKE: &str = "nobody spoke";
pub const CANT_MAKE_OUT: &str = "can't make out (human)";

/// One row of `corrections`, as plain data.
#[derive(Debug, Clone, PartialEq)]
pub struct Correction {
    pub id: i64,
    pub at: Instant,
    pub clip: ClipId,
    pub span: Span,
    pub original: String,
    pub corrected: String,
    pub speaker: Option<String>,
    pub checked: bool,
    /// Set when a person judged the clip unusable for a voiceprint.
    pub hidden_reason: Option<String>,
}

/// A visible line a person named, or a piece of a split, as plain data.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedLine {
    pub id: i64,
    pub at: Instant,
    pub clip: ClipId,
    pub span: Span,
    pub text: String,
    pub speaker: Option<String>,
    /// The text is a person's (a piece of a corrected line), not the model's.
    pub human_text: bool,
}

/// A line a person hid, as plain data.
#[derive(Debug, Clone, PartialEq)]
pub struct Hide {
    pub id: i64,
    pub at: Instant,
    pub clip: ClipId,
    pub span: Span,
    pub unintelligible: bool,
}

/// Where an act came from, so every one traces back to a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Origin {
    Correction(i64),
    Line(i64),
    Hide(i64),
    Pin,
}

/// The acts recovered, in the order they were done, and every row that
/// became none, with why.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Recovered {
    pub acts: Vec<(Instant, Act, Origin)>,
    pub none: Vec<(Origin, &'static str)>,
}

/// Turn the rows into acts. Pure.
pub fn convert(
    corrections: &[Correction],
    lines: &[NamedLine],
    hides: &[Hide],
    pins: &[(SourceId, Option<Language>)],
) -> Recovered {
    let mut out = Recovered::default();
    for c in corrections {
        correction(c, &mut out);
    }
    for l in lines {
        line(l, &mut out);
    }
    for h in hides {
        let act = if h.unintelligible {
            Act::Unintelligible {
                clip: h.clip,
                span: h.span,
            }
        } else {
            Act::NoSpeech {
                clip: h.clip,
                span: h.span,
            }
        };
        out.acts.push((h.at, act, Origin::Hide(h.id)));
    }
    for (source, language) in pins {
        match language {
            Some(language) => out.acts.push((
                pinned_at(),
                Act::Language {
                    source: source.clone(),
                    language: *language,
                },
                Origin::Pin,
            )),
            None => out
                .none
                .push((Origin::Pin, "a pinned language recall does not speak")),
        }
    }
    out.acts.sort_by_key(|(at, _, _)| *at);
    out
}

/// A correction is words (changed or vouched for), a name, or both.
fn correction(c: &Correction, out: &mut Recovered) {
    let origin = Origin::Correction(c.id);
    let changed = c.corrected.trim() != c.original.trim();
    if !changed && !c.checked && c.speaker.is_none() {
        out.none
            .push((origin, "same words, no speaker, not checked: nothing done"));
        return;
    }
    if c.checked && c.corrected.trim().is_empty() {
        // Vouched blank: the person listened and nobody spoke (the Check page
        // records "nobody spoke" as a checked correction to no words).
        out.acts.push((
            c.at,
            Act::NoSpeech {
                clip: c.clip,
                span: c.span,
            },
            origin,
        ));
    } else if changed || c.checked {
        match Text::new(&c.corrected) {
            Some(text) => out.acts.push((
                c.at,
                Act::Words {
                    clip: c.clip,
                    span: c.span,
                    text,
                    checked: true,
                },
                origin,
            )),
            None => out.none.push((origin, "corrected text is blank")),
        }
    }
    if let Some(speaker) = c.speaker.as_deref() {
        match Name::new(speaker) {
            Some(name) => out.acts.push((
                c.at,
                Act::Speaker {
                    clip: c.clip,
                    span: c.span,
                    name,
                    enrol: c.hidden_reason.is_none(),
                },
                origin,
            )),
            None => out.none.push((origin, "speaker is blank")),
        }
    }
}

/// A named line is a name; a piece of a corrected line also carries words.
fn line(l: &NamedLine, out: &mut Recovered) {
    let origin = Origin::Line(l.id);
    let words = l.human_text.then(|| Text::new(&l.text)).flatten();
    let name = l.speaker.as_deref().and_then(Name::new);
    if words.is_none() && name.is_none() {
        out.none
            .push((origin, "a person's line with neither words nor a name"));
        return;
    }
    if let Some(text) = words {
        out.acts.push((
            l.at,
            Act::Words {
                clip: l.clip,
                span: l.span,
                text,
                checked: true,
            },
            origin,
        ));
    }
    if let Some(name) = name {
        out.acts.push((
            l.at,
            Act::Speaker {
                clip: l.clip,
                span: l.span,
                name,
                enrol: true,
            },
            origin,
        ));
    }
}

/// "Time unknown": a pin's time is not recorded, nor are the earliest rows'.
/// A pin is about a whole session, so it never competes with an act over a
/// span; an undated row loses to any dated act over the same span.
fn pinned_at() -> Instant {
    Instant::from_utc(chrono::DateTime::UNIX_EPOCH)
}

/// A row that could not be read as one of the plain structs.
#[derive(Debug)]
pub enum LoadError {
    Db(rusqlite::Error),
    Clip(ClipError),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(err) => write!(f, "{err}"),
            Self::Clip(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<rusqlite::Error> for LoadError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err)
    }
}

impl From<ClipError> for LoadError {
    fn from(err: ClipError) -> Self {
        Self::Clip(err)
    }
}

/// The rows, read. A row whose clip, span or time cannot be read is listed in
/// `unreadable`, never dropped silently.
#[derive(Debug, Default)]
pub struct Loaded {
    pub corrections: Vec<Correction>,
    pub lines: Vec<NamedLine>,
    pub hides: Vec<Hide>,
    pub pins: Vec<(SourceId, Option<Language>)>,
    pub unreadable: Vec<(Origin, String)>,
}

pub fn load(meaning: &Connection, ingest: &Connection, root: &Path) -> Result<Loaded, LoadError> {
    let at = Anchor { ingest, root };
    let mut out = Loaded::default();
    load_corrections(meaning, &at, &mut out)?;
    load_lines(meaning, &at, &mut out)?;
    load_hides(meaning, &at, &mut out)?;
    load_pins(meaning, &mut out)?;
    Ok(out)
}

/// Where a row sits: its clip, when it was done, and its span.
struct Anchor<'a> {
    ingest: &'a Connection,
    root: &'a Path,
}

struct Placed {
    clip: ClipId,
    at: Instant,
    span: Span,
}

impl Anchor<'_> {
    /// `Ok(Err(why))` for a row that cannot be placed; `Err` only for the store.
    /// A row with no recorded time (the earliest ones) is placed at the
    /// epoch, so any dated act over the same span wins; [`Census::undated`]
    /// counts them.
    fn place(
        &self,
        path: &str,
        at: Option<&str>,
        start: &str,
        end: &str,
    ) -> Result<Result<Placed, String>, LoadError> {
        let Some(clip) = clips::for_audio_path(self.ingest, self.root, path)? else {
            return Ok(Err(format!("no clip for {path}")));
        };
        let at = match at {
            None => Some(pinned_at()),
            Some(text) => Instant::parse(text),
        };
        let (Some(at), Some(start), Some(end)) = (at, Instant::parse(start), Instant::parse(end))
        else {
            return Ok(Err(format!(
                "unreadable time among {at:?} {start:?} {end:?}"
            )));
        };
        Ok(Span::new(start, end)
            .map(|span| Placed {
                clip: clip.id,
                at,
                span,
            })
            .ok_or_else(|| format!("backwards span {start:?}..{end:?}")))
    }
}

/// The columns every row kind starts with: id, when, the clip's path, the span.
fn head(r: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, Option<String>, String, String, String)> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

fn load_corrections(
    meaning: &Connection,
    anchor: &Anchor<'_>,
    out: &mut Loaded,
) -> Result<(), LoadError> {
    let mut stmt = CORRECTIONS.prepare(meaning)?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let (id, at, path, start, end) = head(r)?;
        match anchor.place(&path, at.as_deref(), &start, &end)? {
            Ok(Placed { clip, at, span }) => out.corrections.push(Correction {
                id,
                at,
                clip,
                span,
                original: r.get(5)?,
                corrected: r.get(6)?,
                speaker: r.get(7)?,
                checked: r.get::<_, Option<bool>>(8)?.unwrap_or(false),
                hidden_reason: r.get(9)?,
            }),
            Err(why) => out.unreadable.push((Origin::Correction(id), why)),
        }
    }
    Ok(())
}

fn load_lines(
    meaning: &Connection,
    anchor: &Anchor<'_>,
    out: &mut Loaded,
) -> Result<(), LoadError> {
    let mut stmt = NAMED_LINES.prepare(meaning)?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let (id, at, path, start, end) = head(r)?;
        match anchor.place(&path, at.as_deref(), &start, &end)? {
            Ok(Placed { clip, at, span }) => out.lines.push(NamedLine {
                id,
                at,
                clip,
                span,
                text: r.get(5)?,
                speaker: r.get(6)?,
                human_text: r.get::<_, Option<bool>>(7)?.unwrap_or(false),
            }),
            Err(why) => out.unreadable.push((Origin::Line(id), why)),
        }
    }
    Ok(())
}

fn load_hides(
    meaning: &Connection,
    anchor: &Anchor<'_>,
    out: &mut Loaded,
) -> Result<(), LoadError> {
    let mut stmt = HIDES.prepare(meaning)?;
    let mut rows = stmt.query((NOBODY_SPOKE, CANT_MAKE_OUT))?;
    while let Some(r) = rows.next()? {
        let (id, at, path, start, end) = head(r)?;
        let reason: String = r.get(5)?;
        match anchor.place(&path, at.as_deref(), &start, &end)? {
            Ok(Placed { clip, at, span }) => out.hides.push(Hide {
                id,
                at,
                clip,
                span,
                unintelligible: reason == CANT_MAKE_OUT,
            }),
            Err(why) => out.unreadable.push((Origin::Hide(id), why)),
        }
    }
    Ok(())
}

fn load_pins(meaning: &Connection, out: &mut Loaded) -> Result<(), LoadError> {
    let mut stmt = PINS.prepare(meaning)?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let (source, code): (String, String) = (r.get(0)?, r.get(1)?);
        match SourceId::parse(&source) {
            Some(id) => out.pins.push((id, Language::from_code(&code))),
            None => out
                .unreadable
                .push((Origin::Pin, format!("source {source:?} is not a source id"))),
        }
    }
    Ok(())
}

/// Every human row, and what it became.
#[derive(Debug, Default, serde::Serialize)]
pub struct Census {
    pub corrections: usize,
    pub lines: usize,
    pub hides: usize,
    pub pins: usize,
    pub acts: std::collections::BTreeMap<&'static str, usize>,
    pub none: std::collections::BTreeMap<&'static str, usize>,
    /// Acts from rows with no recorded time, placed first.
    pub undated: usize,
    /// Rows that could not be placed on a clip and a span, with why.
    pub unreadable: Vec<(Origin, String)>,
}

pub fn census(meaning: &Connection, ingest: &Connection, root: &Path) -> Result<Census, LoadError> {
    let loaded = load(meaning, ingest, root)?;
    let recovered = convert(
        &loaded.corrections,
        &loaded.lines,
        &loaded.hides,
        &loaded.pins,
    );
    let mut census = Census {
        corrections: loaded.corrections.len(),
        lines: loaded.lines.len(),
        hides: loaded.hides.len(),
        pins: loaded.pins.len(),
        unreadable: loaded.unreadable,
        ..Census::default()
    };
    for (at, act, _) in &recovered.acts {
        *census.acts.entry(kind(act)).or_default() += 1;
        if *at == pinned_at() && !matches!(act, Act::Language { .. }) {
            census.undated += 1;
        }
    }
    for (_, why) in &recovered.none {
        *census.none.entry(why).or_default() += 1;
    }
    Ok(census)
}

fn kind(act: &Act) -> &'static str {
    match act {
        Act::Words { .. } => "words",
        Act::Speaker { .. } => "speaker",
        Act::NoSpeech { .. } => "no-speech",
        Act::Unintelligible { .. } => "unintelligible",
        Act::Voice { .. } => "voice",
        Act::Language { .. } => "language",
        Act::Retract { .. } => "retract",
    }
}
