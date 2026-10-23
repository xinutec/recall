//! Folding a stream of turns into something browsable.
//!
//! A *conversation* is a maximal run with no silence longer than `gap`. Inside
//! one, each shown line is a *moment*, carrying the other microphones' versions
//! of the same speech.

use crate::reads::Attribution;
use crate::same_speech::same_span;
use std::collections::HashMap;
use transcript::{Instant, Name, SourceId, Span};

/// A conversation breaks after a silence longer than this; the `gap` query
/// parameter overrides it.
pub const DEFAULT_GAP_SECONDS: f64 = 300.0;

/// A turn reduced to what folding reads.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub id: i64,
    pub span: Span,
    pub speaker: Option<Attribution>,
    pub source_id: Option<SourceId>,
    pub asr_confidence: Option<f64>,
    /// The clip it came from: a mic's two clips of one span are copies.
    pub audio_segment_id: Option<i64>,
    /// Its words as [`spoken`] spells them, each with its start: what
    /// [`clock_offsets`] matches across mics.
    pub words: Vec<(Instant, String)>,
}

/// How far a mic's clock sits from the reference mic's: what to add to its
/// stamps to land on the reference's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClockOffset {
    micros: i64,
}

impl ClockOffset {
    #[must_use]
    pub const fn micros(self) -> i64 {
        self.micros
    }
}

/// How far apart two mics' clocks are searched for: phones have been seen
/// a few seconds out.
const MAX_OFFSET_US: i64 = 10_000_000;

/// Word time differences this close count as one offset: a model times a
/// word to about a tenth of a second.
const AGREE_US: i64 = 250_000;

/// The fewest words that must agree on an offset before a clock moves: a
/// short reply on its own matches anything.
const MIN_AGREEING_WORDS: usize = 4;

/// A word as matched across mics: lower case, letters and digits only.
#[must_use]
pub fn spoken(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Each source's clock offset in microseconds, to add to its stamps to land on
/// the clock of the source with the most words. Taken from the words both
/// heard: the time difference most of them agree on. A source with fewer than
/// [`MIN_AGREEING_WORDS`] agreeing keeps its own clock, so it is absent.
#[must_use]
pub fn clock_offsets<'a>(turns: &'a [Turn], group: &[usize]) -> HashMap<&'a SourceId, ClockOffset> {
    let mut order: Vec<&SourceId> = Vec::new();
    let mut words: HashMap<&SourceId, Vec<(i64, &str)>> = HashMap::new();
    for &i in group {
        let Some(source) = turns[i].source_id.as_ref() else {
            continue;
        };
        let heard = words.entry(source).or_insert_with(|| {
            order.push(source);
            Vec::new()
        });
        heard.extend(
            turns[i]
                .words
                .iter()
                .filter(|(_, w)| !w.is_empty())
                .map(|(t, w)| (t.micros(), w.as_str())),
        );
    }
    // The most words, the first on a tie.
    let Some(reference) = order.iter().copied().reduce(|best, s| {
        if words[s].len() > words[best].len() {
            s
        } else {
            best
        }
    }) else {
        return HashMap::new();
    };
    let mut at: HashMap<&str, Vec<i64>> = HashMap::new();
    for &(t, w) in &words[reference] {
        at.entry(w).or_default().push(t);
    }
    for times in at.values_mut() {
        times.sort_unstable();
    }
    let mut out = HashMap::new();
    for &source in order.iter().filter(|&&s| s != reference) {
        let mut deltas: Vec<i64> = Vec::new();
        for &(t, w) in &words[source] {
            let Some(times) = at.get(w) else { continue };
            let from = times.partition_point(|&r| r < t - MAX_OFFSET_US);
            let to = times.partition_point(|&r| r <= t + MAX_OFFSET_US);
            deltas.extend(times[from..to].iter().map(|&r| r - t));
        }
        deltas.sort_unstable();
        // The window of agreeing differences holding the most, the first on
        // a tie; its median is the offset.
        let (mut best, mut end) = ((0, 0), 0);
        for start in 0..deltas.len() {
            while end < deltas.len() && deltas[end] - deltas[start] <= 2 * AGREE_US {
                end += 1;
            }
            if end - start > best.1 - best.0 {
                best = (start, end);
            }
        }
        if best.1 - best.0 >= MIN_AGREEING_WORDS {
            let micros = deltas[usize::midpoint(best.0, best.1)];
            out.insert(source, ClockOffset { micros });
        }
    }
    out
}

/// The turns with the group's on one clock ([`clock_offsets`]), for deciding
/// which lines are the same speech; what is shown keeps its stored times.
#[must_use]
pub fn aligned(turns: &[Turn], group: &[usize]) -> Vec<Turn> {
    let offsets = clock_offsets(turns, group);
    let mut out = turns.to_vec();
    for &i in group {
        let Some(&offset) = turns[i].source_id.as_ref().and_then(|s| offsets.get(s)) else {
            continue;
        };
        // An offset is a few seconds, nowhere near the edge of time.
        let Some(span) = turns[i].span.shifted(offset.micros()) else {
            continue;
        };
        out[i].span = span;
    }
    out
}

/// Split turns sorted by start into conversations, as index groups. Silence is
/// measured from the running maximum end: turns from several mics overlap.
pub fn segment_conversations(turns: &[Turn], gap_seconds: f64) -> Vec<Vec<usize>> {
    let mut conversations = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let gap_micros = gap_seconds * 1e6;
    let mut prev_end: Option<Instant> = None;
    for (index, turn) in turns.iter().enumerate() {
        if let Some(end) = prev_end
            && (turn.span.start().micros() - end.micros()) as f64 > gap_micros
        {
            conversations.push(std::mem::take(&mut current));
            prev_end = None;
        }
        current.push(index);
        let end = turn.span.end();
        prev_end = Some(prev_end.map_or(end, |last| last.max(end)));
    }
    if !current.is_empty() {
        conversations.push(current);
    }
    conversations
}

/// One shown line, with the other mics' versions of it (and its own mic's
/// second copy) for the compare view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moment {
    pub primary: usize,
    pub alternates: Vec<usize>,
}

/// Fold one conversation's turns into moments.
///
/// The conversation is cut into stretches of overlapping speech, each shown
/// from the mic that heard it best ([`spine`]). Every other line goes with the
/// shown line it overlaps most (phone clocks lag a few seconds); a line that
/// overlaps none is speech the shown mic missed, and is shown too.
pub fn cluster_moments(turns: &[Turn], group: &[usize]) -> Vec<Moment> {
    let mut stretches: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut running_end: Option<Instant> = None;
    for &index in group {
        let turn = &turns[index];
        match running_end {
            Some(end) if turn.span.start() < end => {
                current.push(index);
                running_end = Some(end.max(turn.span.end()));
            }
            _ => {
                if !current.is_empty() {
                    stretches.push(std::mem::take(&mut current));
                }
                current = vec![index];
                running_end = Some(turn.span.end());
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
        let mut best: Option<(usize, i64)> = None;
        for (k, moment) in moments.iter().enumerate() {
            let shared = turns[other].span.shared_micros(turns[moment.primary].span);
            if shared > 0 && best.is_none_or(|(_, most)| shared > most) {
                best = Some((k, shared));
            }
        }
        match best {
            Some((k, _)) => moments[k].alternates.push(other),
            None => missed.push(other),
        }
    }
    // What the shown mic missed: the best-heard version is shown, the others
    // of its span go with it.
    missed.sort_by(|&a, &b| quality(turns, &[b]).total_cmp(&quality(turns, &[a])));
    let mut found: Vec<Moment> = Vec::new();
    for other in missed {
        let heard = turns[other].span;
        match found
            .iter_mut()
            .find(|m| same_span(heard, turns[m.primary].span))
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
        moment.alternates.sort_by_key(|&i| turns[i].span.start());
    }
    moments.sort_by_key(|m| turns[m.primary].span.start());
    moments
}

/// In microseconds, at least a millisecond, so a zero-length turn still
/// weighs something.
fn duration(turn: &Turn) -> f64 {
    turn.span.micros().max(1_000) as f64
}

/// ASR confidence weighted by duration; a missing score counts as zero.
fn quality(turns: &[Turn], indices: &[usize]) -> f64 {
    let total: f64 = indices.iter().map(|&i| duration(&turns[i])).sum();
    let heard: f64 = indices
        .iter()
        .map(|&i| turns[i].asr_confidence.unwrap_or(0.0) * duration(&turns[i]))
        .sum();
    if total > 0.0 { heard / total } else { 0.0 }
}

/// Microseconds covered by the union of these turns' spans.
fn covered(turns: &[Turn], indices: &[usize]) -> i64 {
    let mut spans: Vec<Span> = indices.iter().map(|&i| turns[i].span).collect();
    spans.sort();
    let mut total = 0;
    let mut open: Option<Span> = None;
    for span in spans {
        match open {
            Some(run) if span.start() <= run.end() => open = Some(run.cover(span)),
            _ => {
                total += open.map_or(0, Span::micros);
                open = Some(span);
            }
        }
    }
    total + open.map_or(0, Span::micros)
}

/// One mic's turns split into shown and copies. A phone's minute arrives twice
/// (the Mac's cut of its stream and the phone's own file), and both are
/// transcribed; of clips with the [`same_span`], the better-heard is shown.
fn without_copies(turns: &[Turn], indices: &[usize]) -> (Vec<usize>, Vec<usize>) {
    let mut clips: Vec<(Option<i64>, Vec<usize>)> = Vec::new();
    for &i in indices {
        let clip = turns[i].audio_segment_id;
        match clips.iter_mut().find(|(c, _)| *c == clip) {
            Some((_, members)) => members.push(i),
            None => clips.push((clip, vec![i])),
        }
    }
    let span = |members: &[usize]| members.iter().map(|&i| turns[i].span).reduce(Span::cover);
    clips.sort_by(|a, b| quality(turns, &b.1).total_cmp(&quality(turns, &a.1)));
    let mut kept: Vec<Span> = Vec::new();
    let (mut shown, mut copies) = (Vec::new(), Vec::new());
    for (_, members) in clips {
        let Some(heard) = span(&members) else {
            continue;
        };
        let copy = kept.iter().any(|&kept| same_span(heard, kept));
        if copy {
            copies.extend(members);
        } else {
            kept.push(heard);
            shown.extend(members);
        }
    }
    (shown, copies)
}

/// The stretch's shown lines, from the mic that heard it best, and the rest:
/// the other mics' lines and the shown mic's second copies.
fn spine(turns: &[Turn], stretch: &[usize]) -> (Vec<usize>, Vec<usize>) {
    // First-appearance order; ties below go to the first.
    let mut order: Vec<Option<&str>> = Vec::new();
    let mut by_source: HashMap<Option<&str>, Vec<usize>> = HashMap::new();
    for &index in stretch {
        let source = turns[index].source_id.as_ref().map(SourceId::as_str);
        if !by_source.contains_key(&source) {
            order.push(source);
        }
        by_source.entry(source).or_default().push(index);
    }
    let split: HashMap<Option<&str>, (Vec<usize>, Vec<usize>)> = order
        .iter()
        .map(|&source| (source, without_copies(turns, &by_source[&source])))
        .collect();

    // The mic that heard the stretch best: duration-weighted confidence times
    // coverage, so neither saying more nor a clear fragment wins. Ties go to
    // more turns (the finer speaker split), then the first source; hence a
    // loop, as `max_by_key` keeps the last.
    let span = covered(turns, stretch);
    let key = |source: Option<&str>| {
        let shown = &split[&source].0;
        let coverage = if span > 0 {
            covered(turns, shown) as f64 / span as f64
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
    primary.sort_by_key(|&i| turns[i].span.start());
    let rest: Vec<usize> = order
        .iter()
        .filter(|&&source| source != best)
        .flat_map(|source| by_source[source].iter().copied())
        .chain(copies.iter().copied())
        .collect();
    (primary, rest)
}

/// Who a shown line is put down to, helped by its overlapping alternates:
/// another mic may carry a stronger voiceprint match. A person's name stands.
///
/// A missing guess is filled from the most confident overlap; an existing one
/// only has its score raised by mics naming the same person, never replaced,
/// since lagging phone clocks make overlap weak proof of the same speaker.
#[must_use]
pub fn shown_speaker(turns: &[Turn], line: usize, alternates: &[usize]) -> Option<Attribution> {
    let turn = &turns[line];
    let guesses = alternates
        .iter()
        .map(|&i| &turns[i])
        .filter(|alt| alt.span.overlaps(turn.span))
        .filter_map(|alt| match &alt.speaker {
            Some(Attribution::Guessed { name, score }) => Some((name, *score)),
            _ => None,
        });
    match &turn.speaker {
        Some(Attribution::Named(_)) => turn.speaker.clone(),
        Some(Attribution::Guessed { name, score }) => Some(Attribution::Guessed {
            name: name.clone(),
            score: guesses
                .filter(|(other, _)| *other == name)
                .fold(*score, |most, (_, s)| most.max(s)),
        }),
        // The first on a tie.
        None => guesses
            .reduce(|best, g| if g.1 > best.1 { g } else { best })
            .map(|(name, score)| Attribution::Guessed {
                name: name.clone(),
                score,
            }),
    }
}

// --- the HTTP surface -------------------------------------------------------

use crate::{reads, route};
use audiocore::instant::Stamp;
use axum::extract::Query;
use axum::response::Response;
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

/// A card is headed by its first line at least this confident.
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

fn turns_of(segments: &[reads::Segment]) -> (Vec<Turn>, Vec<usize>) {
    let mut turns = Vec::with_capacity(segments.len());
    let mut kept = Vec::with_capacity(segments.len());
    for (index, segment) in segments.iter().enumerate() {
        turns.push(Turn {
            id: segment.id,
            span: segment.span,
            speaker: segment.speaker.clone(),
            source_id: segment.source_id.clone(),
            asr_confidence: segment.asr_confidence,
            audio_segment_id: segment.audio_segment_id,
            words: segment
                .words
                .iter()
                // A word time outside chrono's range is no word time.
                .filter_map(|w| Some((Instant::from_micros(w.start_us)?, spoken(&w.text))))
                .collect(),
        });
        kept.push(index);
    }
    (turns, kept)
}

fn moment_out(
    segments: &[reads::Segment],
    turns: &[Turn],
    kept: &[usize],
    moment: &Moment,
) -> MomentOut {
    let row = |i: usize| &segments[kept[i]];
    let line = row(moment.primary);
    let speaker = shown_speaker(turns, moment.primary, &moment.alternates);
    // The stored spelling, not chrono's: it trims trailing zeros (.960 for
    // .960000), changing every timestamp on the wire.
    MomentOut {
        start: line.start_utc.to_string(),
        end: line.end_utc.to_string(),
        primary: reads::to_out_with(line, speaker.as_ref()),
        alternates: moment
            .alternates
            .iter()
            .map(|&i| reads::to_out(row(i)))
            .collect(),
        sources: distinct(
            std::iter::once(moment.primary)
                .chain(moment.alternates.iter().copied())
                .map(|i| turns[i].source_id.as_ref().map(SourceId::as_str)),
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
    let on_one_clock = aligned(turns, group);
    // Folding reads the group in start order, which moving a clock changes.
    let mut by_start = group.to_vec();
    by_start.sort_by_key(|&i| (on_one_clock[i].span.start(), on_one_clock[i].id));
    let moments = cluster_moments(&on_one_clock, &by_start);
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
            .map(|&i| row(i).start_utc.to_string())
            .unwrap_or_default(),
        // The latest end, the first on a tie: `max_by_key` keeps the last.
        end: group
            .iter()
            .rev()
            .copied()
            .max_by_key(|&i| turns[i].span.end())
            .map(|i| row(i).end_utc.to_string())
            .unwrap_or_default(),
        turn_count: shown.len(),
        speakers: distinct(group.iter().map(|&i| {
            turns[i]
                .speaker
                .as_ref()
                .and_then(Attribution::named)
                .map(Name::as_str)
        })),
        preview,
        moments: moments
            .iter()
            .map(|moment| moment_out(segments, &on_one_clock, kept, moment))
            .collect(),
    }
}

/// Fold a page of stored turns into conversations. `segments` must be oldest
/// first; `reads::recent` answers newest-first unless paging forward.
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
    /// Instants, re-spelled as stored: a bad one is a 400 from the extractor.
    before: Option<Stamp>,
    after: Option<Stamp>,
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
    let root = st.root.clone();
    let limit = q.limit.clamp(0, 1000);
    route::json("conversations", move || {
        let conn = reads::open(&root)?;
        let mut segments = reads::recent(
            &conn,
            limit,
            reads::Window {
                before: q.before.as_ref(),
                after: q.after.as_ref(),
                source: q.source.as_deref(),
                hidden: q.hidden,
            },
        )?;
        if q.after.is_none() {
            segments.reverse();
        }
        Ok(fold(&segments, q.gap, limit))
    })
    .await
}
