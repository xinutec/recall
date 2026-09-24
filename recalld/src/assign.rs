//! Assigning a span of the transcript to a speaker.
//!
//! Changing who-said-what is one operation: assign a text span, inside a turn
//! or across several with partial edges, to a name. The turns are split at the
//! span's edges and the pieces inside take the name. The frontend coalesces
//! same-speaker neighbours, so merging needs no surgery of its own.
//!
//! Splitting hides the original turn (`hidden_reason`) rather than deleting it,
//! so a wrong split is recoverable. Nothing here removes a row.
//!
//! ⚠ Every offset is a character index, never a byte one: the frontend counts
//! UTF-16 units, which match `char`s for BMP text. Byte arithmetic on a turn
//! with an accented character would cut in the wrong place or panic
//! mid-character, so everything below works on `Vec<char>`.

use crate::turn_store::{self, HiddenReason, NewTurn, Provenance, Stage};
use audiocore::instant;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction};

/// Floor on a split piece's duration, so a collapsed cut never makes a
/// zero-length, audio-less turn (a word that aligned to no audio).
const MIN_PIECE_MS: i64 = 50;

/// One word with its turn-relative timing, as stored: `{s,e,w}`. No
/// `probability`: the stored shape does not carry one.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Word {
    pub s: f64,
    pub e: f64,
    pub w: String,
}

/// The turn a split reads and rewrites.
#[derive(Debug, Clone)]
pub struct Turn {
    pub id: i64,
    pub audio_segment_id: Option<i64>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub text: String,
    pub language: Option<String>,
    pub language_confidence: Option<f64>,
    pub asr_confidence: Option<f64>,
    pub asr_model: Option<String>,
    pub speaker_label: Option<String>,
    pub speaker_cluster: Option<String>,
    pub provenance: Option<String>,
    pub words: Option<Vec<Word>>,
}

/// One piece of a split, before it is written.
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub text: String,
    pub speaker: Option<String>,
    pub words: Option<Vec<Word>>,
}

/// Move `at` to the nearest space, so a split never bisects a word.
///
/// Operates on characters. Ties go left.
fn snap_to_word(chars: &[char], at: usize) -> usize {
    let at = at.min(chars.len());
    if at == 0 || at == chars.len() {
        return at;
    }
    if chars[at - 1].is_whitespace() || chars[at].is_whitespace() {
        return at;
    }
    let left = chars[..at].iter().rposition(|c| *c == ' ');
    let right = chars[at..].iter().position(|c| *c == ' ').map(|i| i + at);
    match (left, right) {
        (None, None) => at,
        (None, Some(right)) => right,
        (Some(left), None) => left,
        (Some(left), Some(right)) => {
            if at - left <= right - at {
                left
            } else {
                right
            }
        }
    }
}

/// The element of `values` closest to `target`; ties take the earliest.
fn nearest(values: &[usize], target: usize) -> usize {
    let mut best = values[0];
    let mut best_gap = best.abs_diff(target);
    for &value in &values[1..] {
        let gap = value.abs_diff(target);
        if gap < best_gap {
            best = value;
            best_gap = gap;
        }
    }
    best
}

/// A float count of seconds as a `Duration`, rounded as Python's
/// `datetime.timedelta` does so stored timestamps match.
///
/// ⚠ Not `(value * 1e6).round()`: the whole seconds come off first and only the
/// fraction is scaled, then rounded half-to-even. On spans of tens of seconds
/// the single-multiply form is one microsecond off.
fn seconds(value: f64) -> Duration {
    let whole = value.trunc();
    let micros = ((value - whole) * 1_000_000.0).round_ties_even() as i64;
    Duration::seconds(whole as i64) + Duration::microseconds(micros)
}

/// Where each word starts, as a character offset into the turn's text paired
/// with its turn-relative time, plus a closing boundary.
///
/// The words' concatenation carries leading whitespace that `turn.text` does
/// not, so every offset is shifted back by that lead; otherwise each cut lands
/// one character late.
fn boundaries(chars: &[char], words: &[Word]) -> Vec<(usize, f64)> {
    let joined: String = words.iter().map(|w| w.w.as_str()).collect();
    let lead = joined.chars().count() - joined.trim_start().chars().count();
    let mut out = Vec::with_capacity(words.len() + 1);
    let mut pos = 0usize;
    for word in words {
        let char_at = pos.saturating_sub(lead).min(chars.len());
        out.push((char_at, word.s));
        pos += word.w.chars().count();
    }
    if let Some(last) = words.last() {
        out.push((chars.len(), last.e));
    }
    out
}

/// Split `turn` at `cuts`, giving each resulting piece the matching speaker.
///
/// `speakers.len()` must be `cuts.len() + 1`. Empty pieces drop out.
pub fn pieces_of(turn: &Turn, cuts: &[usize], speakers: &[Option<String>]) -> Vec<Piece> {
    let chars: Vec<char> = turn.text.chars().collect();
    let span = (turn.end - turn.start).num_microseconds().unwrap_or(0) as f64 / 1_000_000.0;
    let marks = turn.words.as_deref().map(|w| boundaries(&chars, w));

    let bounds: Vec<usize> = {
        let mut out = vec![0usize];
        match &marks {
            Some(marks) if !marks.is_empty() => {
                let offsets: Vec<usize> = marks.iter().map(|(c, _)| *c).collect();
                out.extend(cuts.iter().map(|c| nearest(&offsets, *c)));
            }
            _ => out.extend(cuts.iter().map(|c| snap_to_word(&chars, *c))),
        }
        out.push(chars.len());
        out
    };

    // The turn's own edges are exact, but a word's timestamp can sit inside
    // leading silence or drift, so only interior cuts snap to a word; otherwise
    // the turn's opening or closing audio would be dropped.
    let at = |char_at: usize| -> DateTime<Utc> {
        if char_at == 0 {
            return turn.start;
        }
        if char_at >= chars.len() {
            return turn.end;
        }
        match &marks {
            Some(marks) if !marks.is_empty() => {
                let (_, rel) = marks
                    .iter()
                    .min_by_key(|(c, _)| c.abs_diff(char_at))
                    .expect("non-empty");
                turn.start + seconds(*rel)
            }
            _ => {
                let frac = char_at as f64 / chars.len() as f64;
                turn.start + seconds(span * frac)
            }
        }
    };

    let words_in = |lo: usize, hi: usize, base: f64| -> Option<Vec<Word>> {
        let marks = marks.as_ref()?;
        let words = turn.words.as_ref()?;
        let selected: Vec<Word> = marks
            .iter()
            .take(words.len())
            .zip(words)
            .filter(|((char_at, _), _)| *char_at >= lo && *char_at < hi)
            .map(|(_, word)| Word {
                s: word.s - base,
                e: word.e - base,
                w: word.w.clone(),
            })
            .collect();
        (!selected.is_empty()).then_some(selected)
    };

    let mut pieces = Vec::new();
    for window in bounds.windows(2).enumerate() {
        let (k, pair) = window;
        let (lo, hi) = (pair[0], pair[1]);
        let chunk: String = chars[lo.min(chars.len())..hi.min(chars.len())]
            .iter()
            .collect::<String>()
            .trim()
            .to_owned();
        if chunk.is_empty() {
            continue;
        }
        let start = at(lo);
        let base = (start - turn.start).num_microseconds().unwrap_or(0) as f64 / 1_000_000.0;
        pieces.push(Piece {
            start,
            end: at(hi),
            text: chunk,
            speaker: speakers.get(k).and_then(Clone::clone),
            words: words_in(lo, hi, base),
        });
    }
    pieces
}

/// Widen any zero- or negative-width piece to a minimum playable span, keeping
/// pieces ordered and clamped to the turn.
pub fn min_width(
    pieces: Vec<Piece>,
    turn_start: DateTime<Utc>,
    turn_end: DateTime<Utc>,
) -> Vec<Piece> {
    let floor = Duration::milliseconds(MIN_PIECE_MS);
    let mut out = Vec::with_capacity(pieces.len());
    let mut cursor = turn_start;
    for piece in pieces {
        let lo = piece.start.max(cursor);
        let mut hi = piece.end.max(lo + floor).min(turn_end);
        let lo = if hi <= lo {
            // Out of room at the turn's end: pull the start back instead.
            hi = hi.max(turn_start + floor).min(turn_end);
            turn_start.max(hi - floor)
        } else {
            lo
        };
        cursor = hi;
        out.push(Piece {
            start: lo,
            end: hi,
            ..piece
        });
    }
    out
}

// --- the store half ----------------------------------------------------------

fn load_turn(tx: &Transaction, id: i64) -> rusqlite::Result<Option<Turn>> {
    tx.query_row(
        "SELECT id, audio_segment_id, start_utc, end_utc, text, language, \
                language_confidence, asr_confidence, asr_model, speaker_label, \
                speaker_cluster, provenance, word_timings \
         FROM transcript_segments WHERE id = ?1",
        [id],
        |r| {
            let start: String = r.get(2)?;
            let end: String = r.get(3)?;
            let raw: Option<String> = r.get(12)?;
            Ok(Turn {
                id: r.get(0)?,
                audio_segment_id: r.get(1)?,
                start: parse(&start),
                end: parse(&end),
                text: r.get(4)?,
                language: r.get(5)?,
                language_confidence: r.get(6)?,
                asr_confidence: r.get(7)?,
                asr_model: r.get(8)?,
                speaker_label: r.get(9)?,
                speaker_cluster: r.get(10)?,
                provenance: r.get(11)?,
                // A malformed timings blob degrades to "no timings", which is the
                // interpolated path — worse cuts, not a failed assignment.
                words: raw.and_then(|v| serde_json::from_str(&v).ok()),
            })
        },
    )
    .optional()
}

fn parse(stored: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(stored).map_or_else(|_| Utc::now(), |t| t.with_timezone(&Utc))
}

fn insert_piece(
    tx: &Transaction,
    turn: &Turn,
    piece: &Piece,
    provenance: &Provenance,
    now: &str,
) -> rusqlite::Result<()> {
    let words = piece.words.as_deref().map(crate::pyjson::dump);
    turn_store::insert(
        tx,
        &NewTurn {
            audio_segment_id: turn.audio_segment_id,
            start_utc: &instant::python_isoformat_utc(piece.start),
            end_utc: &instant::python_isoformat_utc(piece.end),
            text: &piece.text,
            language: turn.language.as_deref(),
            language_confidence: turn.language_confidence,
            asr_confidence: turn.asr_confidence,
            asr_model: turn.asr_model.as_deref(),
            speaker_label: piece.speaker.as_deref(),
            speaker_cluster: turn.speaker_cluster.as_deref(),
            provenance: Some(provenance.clone()),
            word_timings: words.as_deref(),
            created_utc: Some(now),
            ..NewTurn::default()
        },
    )?;
    Ok(())
}

/// Replace one turn with the pieces split at `cuts`. Returns 1 if a turn was
/// touched, 0 if there was nothing to do.
fn recut(
    tx: &Transaction,
    turn_id: i64,
    cuts: &[usize],
    speakers: &[Option<String>],
    now: &str,
) -> rusqlite::Result<i64> {
    let Some(turn) = load_turn(tx, turn_id)? else {
        return Ok(0);
    };
    let pieces = pieces_of(&turn, cuts, speakers);
    if pieces.is_empty() {
        return Ok(0);
    }
    if pieces.len() == 1 {
        // The whole turn is one speaker: relabel in place, no split.
        turn_store::set_label(tx, turn_id, pieces[0].speaker.as_deref())?;
        return Ok(1);
    }
    let pieces = min_width(pieces, turn.start, turn.end);
    if !turn_store::claim(tx, turn_id, &HiddenReason::SplitInto(turn_id))? {
        // A concurrent split won. This caller must not also split it.
        return Ok(0);
    }
    // Pieces keep the parent's stage, so the UI shows them as it showed the turn.
    let parent = turn
        .provenance
        .as_deref()
        .and_then(|p| p.parse::<Provenance>().ok());
    let stage = Stage::of(
        turn.asr_model.as_deref(),
        parent.as_ref(),
        turn.speaker_cluster.is_some(),
    );
    let provenance = if stage == Stage::Diarized {
        Provenance::DiarizedSplit(turn_id)
    } else {
        Provenance::Split(turn_id)
    };
    for piece in &pieces {
        insert_piece(tx, &turn, piece, &provenance, now)?;
    }
    Ok(1)
}

fn session_turn_ids(tx: &Transaction, source: &str) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = tx.prepare(
        "SELECT ts.id FROM transcript_segments ts \
         JOIN audio_segments a ON a.id = ts.audio_segment_id \
         WHERE a.source_id = ?1 AND ts.superseded_by IS NULL \
           AND ts.hidden_reason IS NULL \
         ORDER BY ts.start_utc",
    )?;
    let rows = stmt.query_map([source], |r| r.get(0))?;
    rows.collect()
}

/// The span the caller selected, in turn ids and character offsets.
#[derive(Debug, Clone, Copy)]
pub struct Span {
    pub start_turn: i64,
    pub start_char: usize,
    pub end_turn: i64,
    pub end_char: usize,
}

/// Assign a text span to `name`, returning how many turns were touched.
///
/// One gesture covers three: the whole of a turn is a reassign, part of one turn
/// splits that part out, and a span across turns splits the two edges and
/// relabels everything between.
pub fn assign_span(
    conn: &mut Connection,
    source: &str,
    span: Span,
    name: &str,
    now: &str,
) -> rusqlite::Result<i64> {
    let tx = conn.transaction()?;
    let ids = session_turn_ids(&tx, source)?;
    let (Some(i), Some(j)) = (
        ids.iter().position(|id| *id == span.start_turn),
        ids.iter().position(|id| *id == span.end_turn),
    ) else {
        // A turn that is not in this session, or no longer current.
        return Ok(0);
    };
    // A selection made right-to-left arrives with its ends swapped.
    let (i, j, start_turn, start_char, end_turn, end_char) = if i > j {
        (
            j,
            i,
            span.end_turn,
            span.end_char,
            span.start_turn,
            span.start_char,
        )
    } else {
        (
            i,
            j,
            span.start_turn,
            span.start_char,
            span.end_turn,
            span.end_char,
        )
    };

    let named = Some(name.to_owned());
    let touched = if start_turn == end_turn {
        let keep = load_turn(&tx, start_turn)?.and_then(|t| t.speaker_label);
        recut(
            &tx,
            start_turn,
            &[start_char, end_char],
            &[keep.clone(), named, keep],
            now,
        )?
    } else {
        let start_keep = load_turn(&tx, start_turn)?.and_then(|t| t.speaker_label);
        let end_keep = load_turn(&tx, end_turn)?.and_then(|t| t.speaker_label);
        let mut touched = recut(
            &tx,
            start_turn,
            &[start_char],
            &[start_keep, named.clone()],
            now,
        )?;
        for mid in &ids[i + 1..j] {
            turn_store::set_label(&tx, *mid, Some(name))?;
            touched += 1;
        }
        touched + recut(&tx, end_turn, &[end_char], &[named, end_keep], now)?
    };
    tx.commit()?;
    Ok(touched)
}

// --- HTTP -------------------------------------------------------------------

use crate::{reads, route, work};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize, ts_rs::TS)]
#[ts(export, rename = "AssignSpanRequest")]
#[serde(rename_all = "camelCase")]
pub struct AssignIn {
    start_turn: i64,
    start_char: usize,
    end_turn: i64,
    end_char: usize,
    name: String,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export, rename = "AssignResult")]
pub struct AssignOut {
    touched: i64,
}

pub async fn assign_route(
    State(st): State<Arc<reads::State>>,
    Path(source): Path<String>,
    Json(body): Json<AssignIn>,
) -> Response {
    let name = body.name.trim().to_owned();
    if name.is_empty() {
        // An empty name would relabel the span to nothing, which reads as a
        // speaker called "" rather than as unknown.
        return (StatusCode::BAD_REQUEST, "name required").into_response();
    }
    let root = st.root.clone();
    let now = audiocore::instant::python_isoformat_utc(chrono::Utc::now());
    let span = Span {
        start_turn: body.start_turn,
        start_char: body.start_char,
        end_turn: body.end_turn,
        end_char: body.end_char,
    };
    match route::blocking("assign span", move || {
        let mut conn = work::open_write(&root)?;
        assign_span(&mut conn, &source, span, &name, &now)
    })
    .await
    {
        Ok(touched) => Json(AssignOut { touched }).into_response(),
        Err(response) => response,
    }
}
