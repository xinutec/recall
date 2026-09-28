//! Conversation and moment folding: the structure that makes an always-on
//! stream of turns browsable.
//!
//! Two groupings, in order. A *conversation* is a maximal run with no silence
//! longer than `gap` between turns. Inside one, each line shown is a *moment*,
//! carrying the other microphones' versions of the same speech, since every
//! source transcribes the room independently.
//!
//! The folding is pure (no database): it reads only spans, sources and
//! confidences, so tests construct turns directly.

use crate::same_speech::{overlap, same_span, seconds as seconds_between};
use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// A conversation breaks after a silence longer than this. Five minutes is a
/// starting point, exposed as the `gap` query parameter for calibration.
pub const DEFAULT_GAP_SECONDS: f64 = 300.0;

/// A turn reduced to what folding reads, with its instants parsed once.
///
/// Parsed up front because `best_colocated_guess` compares spans a quadratic
/// number of times, and a parse failure must not surface halfway through a fold.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub id: i64,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub speaker_label: Option<String>,
    pub source_id: Option<String>,
    pub asr_confidence: Option<f64>,
    pub speaker_guess: Option<String>,
    pub speaker_score: Option<f64>,
    /// The clip it came from: a mic's two clips of one span are copies.
    pub audio_segment_id: Option<i64>,
}

/// Split chronologically-ordered turns into conversations on silence gaps.
///
/// Returns index groups into `turns`, which must be sorted ascending by start
/// and already filtered to current, non-hidden turns.
///
/// Silence is measured from the running maximum end, not the previous turn's
/// end: turns from several mics overlap, and a turn that finished early must not
/// create a gap.
pub fn segment_conversations(turns: &[Turn], gap_seconds: f64) -> Vec<Vec<usize>> {
    let mut conversations = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut prev_end: Option<DateTime<Utc>> = None;
    for (index, turn) in turns.iter().enumerate() {
        if let Some(end) = prev_end
            && seconds_between(end, turn.start) > gap_seconds
        {
            conversations.push(std::mem::take(&mut current));
            prev_end = None;
        }
        current.push(index);
        prev_end = Some(prev_end.map_or(turn.end, |end| end.max(turn.end)));
    }
    if !current.is_empty() {
        conversations.push(current);
    }
    conversations
}

/// One line as shown: a line of the mic that heard its stretch best, or a line
/// only other mics heard, with every other mic's version of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moment {
    /// The shown line.
    pub primary: usize,
    /// The other mics' versions of it, and its mic's second copy, for the
    /// compare view.
    pub alternates: Vec<usize>,
}

/// Fold one conversation's turns into moments, one per shown line.
///
/// First a merge-overlapping-intervals sweep cuts the conversation into
/// stretches of overlapping speech, and each stretch is shown from the mic
/// that heard it best ([`spine`]). Every other line then goes with the shown
/// line it overlaps most: phone clocks lag a few seconds, so another mic's
/// version straddles two sentences and must land in one. A line that overlaps
/// no shown line is speech that mic missed, and is shown too, once.
pub fn cluster_moments(turns: &[Turn], group: &[usize]) -> Vec<Moment> {
    let mut stretches: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut running_end: Option<DateTime<Utc>> = None;
    for &index in group {
        let turn = &turns[index];
        match running_end {
            Some(end) if turn.start < end => {
                current.push(index);
                running_end = Some(end.max(turn.end));
            }
            _ => {
                if !current.is_empty() {
                    stretches.push(std::mem::take(&mut current));
                }
                current = vec![index];
                running_end = Some(turn.end);
            }
        }
    }
    if !current.is_empty() {
        stretches.push(current);
    }
    stretches
        .iter()
        .flat_map(|stretch| moments_of(turns, stretch))
        .collect()
}

fn span(turn: &Turn) -> (DateTime<Utc>, DateTime<Utc>) {
    (turn.start, turn.end)
}

/// One stretch's moments, in order.
fn moments_of(turns: &[Turn], stretch: &[usize]) -> Vec<Moment> {
    let (shown, rest) = spine(turns, stretch);
    let mut moments: Vec<Moment> = shown
        .iter()
        .map(|&i| Moment {
            primary: i,
            alternates: Vec::new(),
        })
        .collect();
    let mut missed: Vec<usize> = Vec::new();
    for &other in &rest {
        // The most overlapped shown line; the first on a tie, hence `>`.
        let mut best: Option<(usize, f64)> = None;
        for (k, moment) in moments.iter().enumerate() {
            let shared = overlap(span(&turns[other]), span(&turns[moment.primary]));
            if shared > 0.0 && best.is_none_or(|(_, most)| shared > most) {
                best = Some((k, shared));
            }
        }
        match best {
            Some((k, _)) => moments[k].alternates.push(other),
            None => missed.push(other),
        }
    }
    // What the shown mic missed: the best-heard version is shown, the others
    // of the same span go with it. Stable, so equals keep their order.
    missed.sort_by(|&a, &b| quality(turns, &[b]).total_cmp(&quality(turns, &[a])));
    let mut found: Vec<Moment> = Vec::new();
    for other in missed {
        let heard = span(&turns[other]);
        match found
            .iter_mut()
            .find(|m| same_span(heard, span(&turns[m.primary])))
        {
            Some(moment) => moment.alternates.push(other),
            None => found.push(Moment {
                primary: other,
                alternates: Vec::new(),
            }),
        }
    }
    moments.extend(found);
    for moment in &mut moments {
        moment.alternates.sort_by_key(|&i| turns[i].start);
    }
    moments.sort_by_key(|m| turns[m.primary].start);
    moments
}

/// How long a turn lasts, at least a millisecond so a zero-length turn still
/// weighs something.
fn duration(turn: &Turn) -> f64 {
    seconds_between(turn.start, turn.end).max(0.001)
}

/// ASR confidence weighted by duration, a missing score counting as zero: how
/// well these turns were heard, whatever their number.
fn quality(turns: &[Turn], indices: &[usize]) -> f64 {
    let total: f64 = indices.iter().map(|&i| duration(&turns[i])).sum();
    let heard: f64 = indices
        .iter()
        .map(|&i| turns[i].asr_confidence.unwrap_or(0.0) * duration(&turns[i]))
        .sum();
    if total > 0.0 { heard / total } else { 0.0 }
}

/// Seconds covered by the union of these turns' spans.
fn covered(turns: &[Turn], indices: &[usize]) -> f64 {
    let mut spans: Vec<(DateTime<Utc>, DateTime<Utc>)> = indices
        .iter()
        .map(|&i| (turns[i].start, turns[i].end))
        .collect();
    spans.sort();
    let mut total = 0.0;
    let mut open: Option<(DateTime<Utc>, DateTime<Utc>)> = None;
    for (start, end) in spans {
        match open {
            Some((from, to)) if start <= to => open = Some((from, to.max(end))),
            _ => {
                if let Some((from, to)) = open {
                    total += seconds_between(from, to);
                }
                open = Some((start, end));
            }
        }
    }
    if let Some((from, to)) = open {
        total += seconds_between(from, to);
    }
    total
}

/// One mic's turns with a second copy of the same span set aside.
///
/// A phone's minute arrives twice (the Mac's `.flac` of its stream and the
/// phone's own `.wav`, the same samples) and both are transcribed, so one mic
/// can hold two clips over the same seconds. A clip is a copy when its span is
/// the [`same_span`] as a clip kept already; the better-heard clip is kept
/// first.
fn without_copies(turns: &[Turn], indices: &[usize]) -> (Vec<usize>, Vec<usize>) {
    let mut clips: Vec<(Option<i64>, Vec<usize>)> = Vec::new();
    for &i in indices {
        let clip = turns[i].audio_segment_id;
        match clips.iter_mut().find(|(c, _)| *c == clip) {
            Some((_, members)) => members.push(i),
            None => clips.push((clip, vec![i])),
        }
    }
    let span = |members: &[usize]| {
        let start = members.iter().map(|&i| turns[i].start).min();
        let end = members.iter().map(|&i| turns[i].end).max();
        start.zip(end)
    };
    // Stable, so equal clips keep the order they were heard in.
    clips.sort_by(|a, b| quality(turns, &b.1).total_cmp(&quality(turns, &a.1)));
    let mut kept: Vec<(DateTime<Utc>, DateTime<Utc>)> = Vec::new();
    let (mut shown, mut copies) = (Vec::new(), Vec::new());
    for (_, members) in clips {
        let Some((start, end)) = span(&members) else {
            continue;
        };
        let copy = kept.iter().any(|&kept| same_span((start, end), kept));
        if copy {
            copies.extend(members);
        } else {
            kept.push((start, end));
            shown.extend(members);
        }
    }
    (shown, copies)
}

/// The stretch's shown lines, from the mic that heard it best, and the rest:
/// the other mics' lines and the shown mic's second copies.
fn spine(turns: &[Turn], stretch: &[usize]) -> (Vec<usize>, Vec<usize>) {
    // First-appearance order of each source; the tie rule below depends on it.
    let mut order: Vec<Option<&str>> = Vec::new();
    let mut by_source: HashMap<Option<&str>, Vec<usize>> = HashMap::new();
    for &index in stretch {
        let source = turns[index].source_id.as_deref();
        if !by_source.contains_key(&source) {
            order.push(source);
        }
        by_source.entry(source).or_default().push(index);
    }
    let split: HashMap<Option<&str>, (Vec<usize>, Vec<usize>)> = order
        .iter()
        .map(|&source| (source, without_copies(turns, &by_source[&source])))
        .collect();

    // Spine = the mic that heard the stretch best: its duration-weighted
    // confidence, scaled by how much of the stretch it covers, so neither saying
    // more nor catching a clear fragment wins. Ties go to more turns, the finer
    // speaker split.
    //
    // ⚠ On a full tie the first source wins, hence the strict `>` loop:
    // `max_by_key` would keep the last.
    let span = covered(turns, stretch);
    let key = |source: Option<&str>| {
        let shown = &split[&source].0;
        let coverage = if span > 0.0 {
            covered(turns, shown) / span
        } else {
            1.0
        };
        (quality(turns, shown) * coverage, shown.len())
    };
    let mut best = order[0];
    let mut best_key = key(best);
    for &source in &order[1..] {
        let candidate = key(source);
        let better = candidate
            .0
            .total_cmp(&best_key.0)
            .then(candidate.1.cmp(&best_key.1))
            .is_gt();
        if better {
            best = source;
            best_key = candidate;
        }
    }

    let (shown, copies) = &split[&best];
    let mut primary = shown.clone();
    primary.sort_by_key(|&i| turns[i].start);
    let rest: Vec<usize> = order
        .iter()
        .filter(|&&source| source != best)
        .flat_map(|source| by_source[source].iter().copied())
        .chain(copies.iter().copied())
        .collect();
    (primary, rest)
}

/// The most confident speaker guess for a shown line, among it and its
/// time-overlapping alternates (the same speech caught by other microphones).
/// The line is chosen for the cleanest transcription, but another mic may
/// carry a stronger voiceprint match.
///
/// ⚠ A missing guess is filled from the most confident overlapping version; an
/// existing guess only has its score raised by mics naming the same person, and
/// is never replaced. Phone clocks lag by a variable few seconds, so overlap
/// alone does not prove the same speaker. Only the auto guess is refined, never
/// a human label.
pub fn best_colocated_guess(
    turns: &[Turn],
    line: usize,
    alternates: &[usize],
) -> (Option<String>, Option<f64>) {
    let turn = &turns[line];
    let overlapping: Vec<&Turn> = alternates
        .iter()
        .map(|&i| &turns[i])
        .filter(|alt| alt.speaker_guess.is_some() && alt.start < turn.end && alt.end > turn.start)
        .collect();
    let (mut guess, mut score) = (turn.speaker_guess.clone(), turn.speaker_score);
    match &guess {
        None => {
            // Nothing of our own: fill from the most confident co-located
            // version; the first on a tie, as in [`spine`].
            let mut best: Option<&&Turn> = None;
            for alt in &overlapping {
                let strength = alt.speaker_score.unwrap_or(-1.0);
                if best.is_none_or(|b| strength > b.speaker_score.unwrap_or(-1.0)) {
                    best = Some(alt);
                }
            }
            if let Some(best) = best {
                guess.clone_from(&best.speaker_guess);
                score = best.speaker_score;
            }
        }
        Some(name) => {
            for alt in &overlapping {
                let agrees = alt.speaker_guess.as_deref() == Some(name.as_str());
                if let Some(strength) = alt.speaker_score
                    && agrees
                    && score.is_none_or(|s| strength > s)
                {
                    score = Some(strength);
                }
            }
        }
    }
    (guess, score)
}

// --- the HTTP surface -------------------------------------------------------

use crate::{reads, route};
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One shown line as the app renders it, with the other mics' versions of it
/// for compare.
#[derive(Debug, Serialize, PartialEq, ts_rs::TS)]
#[ts(export, rename = "Moment")]
pub struct MomentOut {
    pub start: String,
    pub end: String,
    pub primary: reads::TranscriptOut,
    pub alternates: Vec<reads::TranscriptOut>,
    pub sources: Vec<String>,
}

/// A gap-segmented run of turns, folded into per-moment cards.
#[derive(Debug, Serialize, PartialEq, ts_rs::TS)]
#[ts(export, rename = "Conversation")]
#[serde(rename_all = "camelCase")]
pub struct ConversationOut {
    pub start: String,
    pub end: String,
    pub turn_count: usize,
    pub speakers: Vec<String>,
    pub preview: String,
    pub moments: Vec<MomentOut>,
}

#[derive(Debug, Serialize, PartialEq, ts_rs::TS)]
#[ts(export, rename = "ConversationPage")]
#[serde(rename_all = "camelCase")]
pub struct ConversationsOut {
    pub items: Vec<ConversationOut>,
    pub has_more: bool,
}

/// A card is headed by its first reasonably-confident line, so a low-confidence
/// guess does not become the thing a person reads first.
const PREVIEW_MIN_CONFIDENCE: f64 = 0.5;

/// Distinct values in first-seen order, skipping absent ones.
fn distinct<'a>(values: impl Iterator<Item = Option<&'a str>>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for value in values.flatten() {
        if !value.is_empty() && !seen.iter().any(|s| s == value) {
            seen.push(value.to_owned());
        }
    }
    seen
}

/// Reduce stored rows to the fields folding reads, parsing each instant once.
///
/// A row whose stored time will not parse is dropped, not defaulted: an epoch
/// would create a huge gap and split the conversation.
fn turns_of(segments: &[reads::Segment]) -> (Vec<Turn>, Vec<usize>) {
    let mut turns = Vec::with_capacity(segments.len());
    let mut kept = Vec::with_capacity(segments.len());
    for (index, segment) in segments.iter().enumerate() {
        let (Ok(start), Ok(end)) = (
            DateTime::parse_from_rfc3339(&segment.start_utc),
            DateTime::parse_from_rfc3339(&segment.end_utc),
        ) else {
            tracing::warn!("turn {} has an unparseable span, skipped", segment.id);
            continue;
        };
        turns.push(Turn {
            id: segment.id,
            start: start.with_timezone(&Utc),
            end: end.with_timezone(&Utc),
            speaker_label: segment.speaker_label.clone(),
            source_id: segment.source_id.clone(),
            asr_confidence: segment.asr_confidence,
            speaker_guess: segment.speaker_guess.clone(),
            speaker_score: segment.speaker_score,
            audio_segment_id: segment.audio_segment_id,
        });
        kept.push(index);
    }
    (turns, kept)
}

/// The index in `indices` whose `pick` value is largest, keeping the first on a
/// tie so the choice is deterministic.
fn extreme(
    turns: &[Turn],
    indices: &[usize],
    pick: impl Fn(&Turn) -> DateTime<Utc>,
    want_max: bool,
) -> Option<usize> {
    let mut best: Option<usize> = None;
    for &i in indices {
        let better = best.is_none_or(|b| {
            let (this, that) = (pick(&turns[i]), pick(&turns[b]));
            if want_max { this > that } else { this < that }
        });
        if better {
            best = Some(i);
        }
    }
    best
}

fn moment_out(
    segments: &[reads::Segment],
    turns: &[Turn],
    kept: &[usize],
    moment: &Moment,
) -> MomentOut {
    let row = |i: usize| &segments[kept[i]];
    let line = row(moment.primary);
    let guess = best_colocated_guess(turns, moment.primary, &moment.alternates);
    // ⚠ Emit the stored text (`reads::iso`), never a re-formatted instant:
    // chrono trims trailing fraction zeros (.960 for .960000), which would
    // change every timestamp on the wire.
    MomentOut {
        start: reads::iso(&line.start_utc),
        end: reads::iso(&line.end_utc),
        primary: reads::to_out_with(line, Some(guess)),
        alternates: moment
            .alternates
            .iter()
            .map(|&i| reads::to_out_with(row(i), None))
            .collect(),
        sources: distinct(
            std::iter::once(moment.primary)
                .chain(moment.alternates.iter().copied())
                .map(|i| turns[i].source_id.as_deref()),
        ),
    }
}

fn conversation_out(
    segments: &[reads::Segment],
    turns: &[Turn],
    kept: &[usize],
    group: &[usize],
) -> ConversationOut {
    let row = |i: usize| &segments[kept[i]];
    let moments = cluster_moments(turns, group);
    // What a person reads: the spine's lines, not every mic's copy.
    let shown: Vec<usize> = moments.iter().map(|m| m.primary).collect();
    let preview = shown
        .iter()
        .map(|&i| row(i))
        .find(|s| {
            !s.text.trim().is_empty() && s.asr_confidence.unwrap_or(0.0) >= PREVIEW_MIN_CONFIDENCE
        })
        .or_else(|| shown.first().map(|&i| row(i)))
        .map(|s| s.text.clone())
        .unwrap_or_default();
    ConversationOut {
        start: group
            .first()
            .map(|&i| reads::iso(&row(i).start_utc))
            .unwrap_or_default(),
        end: extreme(turns, group, |t| t.end, true)
            .map(|i| reads::iso(&row(i).end_utc))
            .unwrap_or_default(),
        turn_count: shown.len(),
        speakers: distinct(group.iter().map(|&i| turns[i].speaker_label.as_deref())),
        preview,
        moments: moments
            .iter()
            .map(|moment| moment_out(segments, turns, kept, moment))
            .collect(),
    }
}

/// Fold a page of stored turns into conversations, ready to serialise.
///
/// ⚠ `segments` must be in chronological order, but `reads::recent` answers
/// newest-first unless paging forward. A reversed page has negative gaps and
/// folds into one conversation.
pub fn fold(segments: &[reads::Segment], gap_seconds: f64, limit: i64) -> ConversationsOut {
    let (turns, kept) = turns_of(segments);
    ConversationsOut {
        items: segment_conversations(&turns, gap_seconds)
            .iter()
            .map(|group| conversation_out(segments, &turns, &kept, group))
            .collect(),
        // `>=`, not `==`: a page extends past `limit` on same-instant ties.
        has_more: i64::try_from(segments.len()).is_ok_and(|n| n >= limit),
    }
}

#[derive(Deserialize)]
pub struct ConversationsQuery {
    #[serde(default = "default_limit")]
    limit: i64,
    before: Option<String>,
    after: Option<String>,
    #[serde(default = "default_gap")]
    gap: f64,
    source: Option<String>,
    /// Include hidden turns, for taking a hide back.
    #[serde(default)]
    hidden: bool,
}

const fn default_limit() -> i64 {
    200
}

const fn default_gap() -> f64 {
    DEFAULT_GAP_SECONDS
}

pub async fn conversations_route(
    axum::extract::State(st): axum::extract::State<Arc<reads::State>>,
    Query(q): Query<ConversationsQuery>,
) -> Response {
    // A malformed cursor is a 400, not a dropped filter, which would silently
    // serve an unbounded page.
    for (name, value) in [("before", &q.before), ("after", &q.after)] {
        if let Some(value) = value
            && DateTime::parse_from_rfc3339(value).is_err()
        {
            return (StatusCode::BAD_REQUEST, format!("{name} must be ISO-8601")).into_response();
        }
    }
    let root = st.root.clone();
    let limit = q.limit.clamp(0, 1000);
    route::json("conversations", move || {
        let conn = reads::open(&root)?;
        let mut segments = reads::recent(
            &conn,
            limit,
            reads::Window {
                before: q.before.as_deref(),
                after: q.after.as_deref(),
                source: q.source.as_deref(),
                hidden: q.hidden,
            },
        )?;
        // Forward paging reads oldest-first; every other page is newest-first
        // and is reversed before folding.
        if q.after.is_none() {
            segments.reverse();
        }
        Ok(fold(&segments, q.gap, limit))
    })
    .await
}
