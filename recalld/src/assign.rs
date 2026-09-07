//! Assigning a span of the transcript to a speaker, ported from
//! `recall.conversation`.
//!
//! A turn is a run of words by one speaker, so changing who-said-what is ONE
//! operation: assign a text span — inside a turn, or across several with partial
//! edges — to a name. The turns are split at the span's edges, the pieces inside
//! it take the name, and same-speaker neighbours read as one because the
//! frontend coalesces them. So *merge* needs no surgery of its own.
//!
//! ⚠ **Splitting is the only surgery, and it HIDES rather than deletes.** The
//! original turn stays in the table with a `hidden_reason`, so a wrong split is
//! recoverable. Nothing here removes a row.
//!
//! ⚠ **Every offset is a CHARACTER index, never a byte one.** The Python indexes
//! `text[lo:hi]` by code point and the frontend counts UTF-16 units; both agree
//! for anything in the BMP, which is all this archive contains. Rust's `&str`
//! indexes by BYTE, so the same arithmetic on a turn containing any accented
//! character — a Dutch word, a clinician's name — would cut in the wrong place
//! and, landing mid-character, PANIC rather than quietly misbehave. Everything
//! below therefore works on `Vec<char>`.

use crate::instant;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction};

/// Floor on a split piece's duration, so a collapsed cut never makes a
/// zero-length, audio-less turn (a word that aligned to no audio).
const MIN_PIECE_MS: i64 = 50;

/// `provenance` prefix written by the diarized refine pass.
const DIARIZED_MARKER: &str = "diarized";
/// …and by the word-aligned one, which also keeps a turn out of the re-diarize
/// work-list.
const ALIGNED_MARKER: &str = "diarized-aligned";

/// One word with its turn-relative timing, as stored: `{s,e,w}`.
///
/// ⚠ `probability` is deliberately absent. The Python carries it in memory and
/// drops it at the JSON boundary, defaulting it to 1.0 on load, so a port that
/// stored it would write a column shape the readers do not expect.
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
/// Operates on characters. Ties go left, matching `left if at - left <= right - at`.
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

/// A float count of seconds as a `Duration`, the way `datetime.timedelta` does it.
///
/// ⚠ **Not `(value * 1e6).round()`.** Python splits the whole seconds off first
/// and multiplies only the FRACTION by a million, then rounds half-to-EVEN. For a
/// span tens of seconds long the single-multiply form loses a bit and lands one
/// microsecond away — which the differential caught on a real turn, as a stored
/// timestamp ending 832531 against 832532.
fn seconds(value: f64) -> Duration {
    let whole = value.trunc();
    let micros = ((value - whole) * 1_000_000.0).round_ties_even() as i64;
    Duration::seconds(whole as i64) + Duration::microseconds(micros)
}

/// Where each word starts, as a character offset into the turn's text paired
/// with its turn-relative time, plus a closing boundary.
///
/// ⚠ The words' concatenation carries leading whitespace that `turn.text` does
/// not, so every offset is shifted back by that lead. Without it every cut in a
/// turn whose ASR emitted a leading space lands one character late.
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

    // ⚠ The turn's own edges are exact. A word's TIMESTAMP can sit inside leading
    // silence or drift, so anchoring the first or last piece to it would drop the
    // turn's opening or closing audio. Only interior cuts snap to a word.
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

/// Hide a turn only if it is still current.
///
/// ⚠ One atomic statement, and that is the whole point. An impatient double-tap
/// fires several assigns at once; both read the turn as live and reach here, and
/// only the caller that wins the claim splits it. Without this each would stamp
/// out its own full set of pieces.
fn claim_hidden(tx: &Transaction, id: i64, reason: &str) -> rusqlite::Result<bool> {
    let changed = tx.execute(
        "UPDATE transcript_segments SET hidden_reason = ?1 \
         WHERE id = ?2 AND hidden_reason IS NULL AND superseded_by IS NULL",
        (reason, id),
    )?;
    Ok(changed == 1)
}

fn set_turn_speaker(tx: &Transaction, id: i64, name: Option<&str>) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE transcript_segments SET speaker_label = ?1 WHERE id = ?2",
        (name, id),
    )?;
    Ok(())
}

fn insert_piece(
    tx: &Transaction,
    turn: &Turn,
    piece: &Piece,
    provenance: &str,
    now: &str,
) -> rusqlite::Result<()> {
    let words = piece.words.as_deref().map(crate::pyjson::dump);
    tx.execute(
        "INSERT INTO transcript_segments \
            (audio_segment_id, start_utc, end_utc, text, language, language_confidence, \
             asr_confidence, asr_model, speaker_label, speaker_cluster, provenance, \
             created_utc, word_timings) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        rusqlite::params![
            turn.audio_segment_id,
            iso(piece.start),
            iso(piece.end),
            piece.text,
            turn.language,
            turn.language_confidence,
            turn.asr_confidence,
            turn.asr_model,
            piece.speaker,
            turn.speaker_cluster,
            provenance,
            now,
            words,
        ],
    )?;
    let new_id = tx.last_insert_rowid();
    // The index is maintained by the writer, not a trigger; a piece missing from
    // it is simply unsearchable.
    tx.execute(
        "INSERT INTO transcript_fts (rowid, text) VALUES (?1, ?2)",
        (new_id, &piece.text),
    )?;
    Ok(())
}

/// An instant spelled the way every stored row is.
fn iso(at: DateTime<Utc>) -> String {
    instant::python_isoformat(&at.to_rfc3339()).unwrap_or_else(|| at.to_rfc3339())
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
        set_turn_speaker(tx, turn_id, pieces[0].speaker.as_deref())?;
        return Ok(1);
    }
    let pieces = min_width(pieces, turn.start, turn.end);
    if !claim_hidden(tx, turn_id, &format!("split into pieces ({turn_id})"))? {
        // A concurrent split won. This caller must not also split it.
        return Ok(0);
    }
    // ⚠ Keep the parent's tier. A split of a diarized turn is still of diarized
    // quality, so the pieces carry the aligned marker: the UI stays "finalized"
    // rather than dropping back to the raw card view, and the aligned prefix
    // keeps them out of the re-diarize work-list.
    let parent_diarized = turn
        .provenance
        .as_deref()
        .is_some_and(|p| p.starts_with(DIARIZED_MARKER));
    let provenance = if parent_diarized {
        format!("{ALIGNED_MARKER} split of #{turn_id}")
    } else {
        format!("split of #{turn_id}")
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
            set_turn_speaker(&tx, *mid, Some(name))?;
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignIn {
    start_turn: i64,
    start_char: usize,
    end_turn: i64,
    end_char: usize,
    name: String,
}

#[derive(Serialize)]
struct AssignOut {
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
    let now = chrono::Utc::now().to_rfc3339();
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
