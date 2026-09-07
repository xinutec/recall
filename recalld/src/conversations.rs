//! Conversation and moment folding (stage F1), ported from `recall.conversations`,
//! `recall.moments` and `api_reads.conversations`.
//!
//! ⚠ **This is the LAST unported read**, and the only one that is more than a
//! query: capture is always on, so the archive is one unbroken stream of turns
//! and this is what gives it structure a person can browse. Two independent
//! groupings, applied in order. First a *conversation* is a maximal run with no
//! silence longer than `gap` between turns. Then, inside one, a *moment* folds
//! the several microphones that heard the same utterance into one card, because
//! every source transcribes the room independently and the raw stream shows the
//! same sentence four times.
//!
//! ⚠ **Pure, and deliberately not given a database.** Both foldings are decided
//! by turn spans, sources and confidences alone, so they are tested by
//! constructing turns rather than a schema — which is what makes the tie rules
//! below testable at all.

use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// A conversation breaks after a silence longer than this. Five minutes is a
/// starting point, exposed as the `gap` query parameter for calibration.
pub const DEFAULT_GAP_SECONDS: f64 = 300.0;

/// A turn reduced to what folding reads, with its instants parsed once.
///
/// ⚠ Parsed up front rather than per comparison: the grouping rules compare
/// spans a quadratic number of times in `best_colocated_guess`, and re-parsing
/// an ISO string inside that loop would be both slow and a place for a parse
/// failure to appear halfway through a fold.
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
}

/// Seconds between two instants, exactly.
///
/// ⚠ Microseconds, not `num_seconds()`: the gap rule is `>` against a threshold
/// a caller can set to any float, so truncating to whole seconds would put turns
/// on the wrong side of a boundary the caller chose deliberately.
fn seconds_between(from: DateTime<Utc>, to: DateTime<Utc>) -> f64 {
    let delta = to - from;
    delta.num_microseconds().map_or_else(
        // Only reachable for spans of ~292 000 years, where "an enormous gap"
        // is the right answer anyway.
        || delta.num_seconds() as f64,
        |micros| micros as f64 / 1_000_000.0,
    )
}

/// Split chronologically-ordered turns into conversations on silence gaps.
///
/// Returns index groups into `turns`, which must be sorted ascending by start
/// and already filtered to current, non-hidden turns.
///
/// ⚠ The silence is measured from the running maximum end, not the previous
/// turn's end. Turns overlap constantly — several mics hear one utterance — and
/// measuring from the last turn's end would manufacture a gap out of a turn that
/// merely finished early.
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

/// One wall-clock moment: the best source's turns, and the other sources'
/// overlapping versions of the same speech.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moment {
    /// The spine's turns — its segmentation kept, so a multi-speaker split it
    /// caught survives the fold.
    pub primary: Vec<usize>,
    /// Every other source's overlapping turns, for the compare view.
    pub alternates: Vec<usize>,
}

/// Fold one conversation's turns into moments.
///
/// The same merge-overlapping-intervals sweep `segment_conversations` uses, at a
/// different scale: every turn overlapping the cluster's running span joins it,
/// so one utterance heard by four mics becomes one moment while sequential
/// utterances stay separate.
pub fn cluster_moments(turns: &[Turn], group: &[usize]) -> Vec<Moment> {
    let mut clusters: Vec<Vec<usize>> = Vec::new();
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
                    clusters.push(std::mem::take(&mut current));
                }
                current = vec![index];
                running_end = Some(turn.end);
            }
        }
    }
    if !current.is_empty() {
        clusters.push(current);
    }
    clusters
        .into_iter()
        .map(|cluster| to_moment(turns, &cluster))
        .collect()
}

/// Summed ASR confidence, treating a missing score as zero.
fn confidence(turns: &[Turn], indices: &[usize]) -> f64 {
    indices
        .iter()
        .map(|&i| turns[i].asr_confidence.unwrap_or(0.0))
        .sum()
}

fn to_moment(turns: &[Turn], cluster: &[usize]) -> Moment {
    // First-appearance order of each source, which is what Python's dict
    // iteration gives and what both tie rules below depend on.
    let mut order: Vec<Option<&str>> = Vec::new();
    let mut by_source: HashMap<Option<&str>, Vec<usize>> = HashMap::new();
    for &index in cluster {
        let source = turns[index].source_id.as_deref();
        if !by_source.contains_key(&source) {
            order.push(source);
        }
        by_source.entry(source).or_default().push(index);
    }

    // Spine = the source with the highest summed confidence (cleaner audio
    // scores higher); ties go to the one with more turns, i.e. the finer
    // speaker split.
    //
    // ⚠ **The FIRST maximum, not the last.** Python's `max` keeps the first
    // among equals and Rust's `max_by_key` keeps the last, so a strict `>` here
    // is not a style choice: on a true tie — equal summed confidence AND equal
    // turn count — the two implementations would choose different microphones as
    // the spine, silently swapping which transcription the UI shows as primary
    // and which it hides behind "compare".
    let mut best = order[0];
    let mut best_key = (confidence(turns, &by_source[&best]), by_source[&best].len());
    for &source in &order[1..] {
        let key = (
            confidence(turns, &by_source[&source]),
            by_source[&source].len(),
        );
        let better = key
            .0
            .total_cmp(&best_key.0)
            .then(key.1.cmp(&best_key.1))
            .is_gt();
        if better {
            best = source;
            best_key = key;
        }
    }

    let mut primary = by_source[&best].clone();
    primary.sort_by_key(|&i| turns[i].start);
    let mut alternates: Vec<usize> = order
        .iter()
        .filter(|&&source| source != best)
        .flat_map(|source| by_source[source].iter().copied())
        .collect();
    alternates.sort_by_key(|&i| turns[i].start);
    Moment {
        primary,
        alternates,
    }
}

/// For each spine turn, the most confident speaker guess among it and the
/// time-overlapping alternates — the same speech caught by other microphones.
///
/// The spine is chosen for the cleanest *transcription*, which says nothing
/// about *attribution*: a co-located mic may carry a stronger voiceprint match
/// for the very same words.
///
/// ⚠ **Identity-preserving, and that asymmetry is the point.** A missing guess
/// is filled from the most confident overlapping version, and an existing guess
/// has its confidence raised only by mics naming the SAME person — but a name is
/// never flipped on a time overlap alone. Phone clocks are arrival-stamped and
/// lag by a variable few seconds, so raw overlap is not a reliable "same
/// speaker" signal; borrowing a different name from it would assert the wrong
/// person, while corroborating the same name only strengthens what is there.
/// Confirmed human labels are untouched — this refines the auto guess only.
pub fn best_colocated_guess(
    turns: &[Turn],
    primary: &[usize],
    alternates: &[usize],
) -> HashMap<i64, (Option<String>, Option<f64>)> {
    let mut chosen = HashMap::new();
    for &index in primary {
        let turn = &turns[index];
        let overlapping: Vec<&Turn> = alternates
            .iter()
            .map(|&i| &turns[i])
            .filter(|alt| {
                alt.speaker_guess.is_some() && alt.start < turn.end && alt.end > turn.start
            })
            .collect();
        let (mut guess, mut score) = (turn.speaker_guess.clone(), turn.speaker_score);
        match &guess {
            None => {
                // Nothing of our own: fill from the most confident co-located
                // version. First maximum again, for the reason in `to_moment`.
                let mut best: Option<&&Turn> = None;
                for alt in &overlapping {
                    let strength = alt.speaker_score.unwrap_or(-1.0);
                    let beaten = best.is_none_or(|b| strength > b.speaker_score.unwrap_or(-1.0));
                    if beaten {
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
        chosen.insert(turn.id, (guess, score));
    }
    chosen
}

// --- the HTTP surface -------------------------------------------------------

use crate::{reads, route};
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One wall-clock moment as the app renders it: the best mic's turn(s), its
/// speaker split kept, plus the other mics' overlapping versions for compare.
#[derive(Debug, Serialize, PartialEq)]
pub struct MomentOut {
    pub start: String,
    pub end: String,
    pub primary: Vec<reads::TranscriptOut>,
    pub alternates: Vec<reads::TranscriptOut>,
    pub sources: Vec<String>,
}

/// A gap-segmented run of turns, folded into per-moment cards.
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationOut {
    pub start: String,
    pub end: String,
    pub turn_count: usize,
    pub speakers: Vec<String>,
    pub preview: String,
    pub moments: Vec<MomentOut>,
}

#[derive(Debug, Serialize, PartialEq)]
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
/// ⚠ A row whose stored time will not parse is DROPPED rather than defaulted.
/// Substituting an epoch would place the turn at the far past, where it would
/// silently split every conversation after it by manufacturing a huge gap.
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
    let guesses = best_colocated_guess(turns, &moment.primary, &moment.alternates);
    let row = |i: usize| &segments[kept[i]];
    // ⚠ Stored text, never a re-formatted instant — see `reads::iso`. chrono
    // trims trailing zeros from the fraction (.960) where Python's isoformat
    // keeps six digits (.960000), so parsing and re-emitting here would change
    // every timestamp on the wire while looking like a no-op.
    MomentOut {
        start: extreme(turns, &moment.primary, |t| t.start, false)
            .map(|i| reads::iso(&row(i).start_utc))
            .unwrap_or_default(),
        end: extreme(turns, &moment.primary, |t| t.end, true)
            .map(|i| reads::iso(&row(i).end_utc))
            .unwrap_or_default(),
        primary: moment
            .primary
            .iter()
            .map(|&i| reads::to_out_with(row(i), guesses.get(&turns[i].id).cloned()))
            .collect(),
        alternates: moment
            .alternates
            .iter()
            .map(|&i| reads::to_out_with(row(i), None))
            .collect(),
        sources: distinct(
            moment
                .primary
                .iter()
                .chain(moment.alternates.iter())
                .map(|&i| turns[i].source_id.as_deref()),
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
    let preview = group
        .iter()
        .map(|&i| row(i))
        .find(|s| {
            !s.text.trim().is_empty() && s.asr_confidence.unwrap_or(0.0) >= PREVIEW_MIN_CONFIDENCE
        })
        .or_else(|| group.first().map(|&i| row(i)))
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
        turn_count: group.len(),
        speakers: distinct(group.iter().map(|&i| turns[i].speaker_label.as_deref())),
        preview,
        moments: cluster_moments(turns, group)
            .iter()
            .map(|moment| moment_out(segments, turns, kept, moment))
            .collect(),
    }
}

/// Fold a page of stored turns into conversations, ready to serialise.
///
/// ⚠ `segments` must be in CHRONOLOGICAL order, which is not how most pages
/// arrive: `reads::recent` answers newest-first unless paging forward. Folding a
/// reversed page computes negative gaps, so every turn lands in one conversation
/// and the moments inside it are grouped by a sweep that never advances.
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
    // ⚠ A malformed cursor is a 400, never a dropped filter: paging on with the
    // bound silently removed would serve the whole archive as one page and read
    // as "the conversation grew", not as an error.
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
            },
        )?;
        // Forward paging already reads oldest-first; every other page is
        // newest-first and has to be turned around before folding.
        if q.after.is_none() {
            segments.reverse();
        }
        Ok(fold(&segments, q.gap, limit))
    })
    .await
}
