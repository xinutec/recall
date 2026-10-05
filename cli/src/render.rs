//! API turns as the lines a person reads. Pure apart from the local offset.

use crate::api::{Conversation, Export, Session, Turn};
use chrono::{DateTime, Local};

/// Lower bounds of the loudness bands clear and quiet; below is faint.
const CLEAR_LOUDNESS: f64 = 0.05;
const QUIET_LOUDNESS: f64 = 0.01;

fn local(iso: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|t| t.with_timezone(&Local))
}

/// `iso` in local time, or verbatim if it does not parse.
fn when(iso: &str, format: &str) -> String {
    local(iso).map_or_else(|| iso.to_owned(), |t| t.format(format).to_string())
}

/// Who said a turn, for a search hit: the confirmed name, else the voiceprint
/// guess with its score ("Name ~76%"), else the diarized voice, else
/// "unknown".
///
/// Unlike [`who`] it shows guesses: most hits are unconfirmed, and would
/// nearly all read "unknown".
#[must_use]
pub fn attribution(turn: &Turn) -> String {
    if turn.speaker_confirmed
        && let Some(name) = &turn.speaker
    {
        return name.clone();
    }
    if let Some(guess) = &turn.speaker {
        let strength = turn
            .speaker_confidence
            .map_or_else(String::new, |s| format!(" ~{}%", (s * 100.0).round()));
        return format!("{guess}{strength}");
    }
    turn.cluster.clone().unwrap_or_else(|| "unknown".to_owned())
}

/// Who said a turn, for a transcript read end to end: the confirmed name,
/// else the diarized voice, else "unknown". No guesses.
#[must_use]
pub fn who(turn: &Turn) -> String {
    if turn.speaker_confirmed
        && let Some(name) = &turn.speaker
    {
        return name.clone();
    }
    turn.cluster.clone().unwrap_or_else(|| "unknown".to_owned())
}

#[must_use]
pub fn hit(turn: &Turn) -> String {
    let lang = turn
        .language
        .as_ref()
        .map_or_else(String::new, |l| format!(" [{l}]"));
    let src = turn
        .source
        .as_ref()
        .map_or_else(String::new, |s| format!(" ({s})"));
    format!(
        "{}  {}{lang}{src}  {}",
        when(&turn.start, "%Y-%m-%d %H:%M:%S"),
        attribution(turn),
        turn.text
    )
}

fn clarity(loudness: Option<f64>) -> &'static str {
    match loudness {
        None => "unmeasured",
        Some(l) if l >= CLEAR_LOUDNESS => "clear",
        Some(l) if l >= QUIET_LOUDNESS => "quiet",
        Some(_) => "faint",
    }
}

/// Attribution for the diagnostic view, tagged confirmed or guess. A confirmed
/// `SPEAKER_*` is a diarization placeholder, not a name.
fn who_detail(turn: &Turn) -> String {
    match &turn.speaker {
        Some(name) if turn.speaker_confirmed && !name.starts_with("SPEAKER_") => {
            format!("{name} (confirmed)")
        }
        Some(guess) if !turn.speaker_confirmed => {
            let pct = turn
                .speaker_confidence
                .map_or_else(String::new, |s| format!(" ~{}%", (s * 100.0).round()));
            format!("{guess}{pct} (guess)")
        }
        _ => "unknown".to_owned(),
    }
}

/// Every field of specific turns.
///
/// `asked` is the ids typed. `/api/transcripts` answers with a turn's current
/// version, so where the id differs the status names the one it replaced.
#[must_use]
pub fn details(asked: &[i64], turns: &[Turn]) -> String {
    turns
        .iter()
        .zip(asked.iter().copied().chain(std::iter::repeat(0)))
        .map(|(t, asked_id)| {
            #[expect(clippy::cast_precision_loss, reason = "a turn lasts seconds")]
            let duration = local(&t.end)
                .zip(local(&t.start))
                .map_or(0.0, |(e, s)| (e - s).num_milliseconds() as f64 / 1000.0);
            let status = match &t.hidden {
                Some(why) => format!("hidden: {why}"),
                None if asked_id != 0 && asked_id != t.id => {
                    format!("current; #{asked_id} was superseded by this")
                }
                None => "visible".to_owned(),
            };
            let conf = t
                .confidence
                .map_or_else(|| "—".to_owned(), |c| format!("{c:.2}"));
            let loud = t
                .loudness
                .map_or_else(|| "—".to_owned(), |l| format!("{l:.5}"));
            [
                format!(
                    "#{}  {}-{}  ({duration:.1}s)  [{}]  src={}",
                    t.id,
                    when(&t.start, "%a %d %b %Y %H:%M:%S"),
                    when(&t.end, "%H:%M:%S"),
                    t.language.as_deref().unwrap_or("?"),
                    t.source.as_deref().unwrap_or("—"),
                ),
                format!(
                    "  who      : {}   voice={}",
                    who_detail(t),
                    t.cluster.as_deref().unwrap_or("—")
                ),
                format!(
                    "  conf     : {conf}   loudness: {loud} ({})",
                    clarity(t.loudness)
                ),
                format!("  model    : {}", t.model.as_deref().unwrap_or("—")),
                format!("  tier     : {}", t.tier),
                format!("  status   : {status}"),
                format!("  text     : {}", t.text),
            ]
            .join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A transcript, one line per turn, so each keeps its timestamp.
#[must_use]
pub fn transcript(title: &str, turns: &[Turn]) -> String {
    let mut header = format!("# {title}");
    if let Some(first) = turns.first() {
        header.push_str(&when(&first.start, "  (%a %d %b %Y %H:%M)"));
    }
    let mut lines = vec![header, String::new()];
    lines.extend(
        turns
            .iter()
            .map(|t| format!("[{}] {}: {}", when(&t.start, "%H:%M:%S"), who(t), t.text)),
    );
    lines.join("\n")
}

/// `1h05m`, `3m07s` or `42s`.
fn duration(start: &str, end: &str) -> String {
    let Some(seconds) = local(start)
        .zip(local(end))
        .map(|(s, e)| (e - s).num_seconds().max(0))
    else {
        return "?".to_owned();
    };
    let (hours, rest) = (seconds / 3600, seconds % 3600);
    let (minutes, secs) = (rest / 60, rest % 60);
    if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{secs:02}s")
    } else {
        format!("{secs}s")
    }
}

#[must_use]
pub fn sessions(items: &[Session]) -> String {
    if items.is_empty() {
        return "no sessions recorded".to_owned();
    }
    items
        .iter()
        .map(|s| {
            let who = if s.speakers.is_empty() {
                "unknown".to_owned()
            } else {
                s.speakers.join(", ")
            };
            format!(
                "{}  {}  {:>7}  {:>4} turns  {who}",
                s.id,
                when(&s.start, "%a %d %b %Y %H:%M"),
                duration(&s.start, &s.end),
                s.turn_count,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A session's export, same-speaker turns already merged by the route.
#[must_use]
pub fn export(export: &Export) -> String {
    let mut header = format!("# {}", export.session);
    if let Some(date) = &export.date {
        header.push_str(&when(date, "  (%a %d %b %Y %H:%M)"));
    }
    if !export.speakers.is_empty() {
        use std::fmt::Write as _;
        let _ = write!(header, "\n# {}", export.speakers.join(", "));
    }
    let mut lines = vec![header, String::new()];
    lines.extend(
        export
            .turns
            .iter()
            .map(|b| format!("[{}] {}: {}", when(&b.start, "%H:%M:%S"), b.speaker, b.text)),
    );
    lines.join("\n")
}

/// A day's conversations, numbered for `--show`.
#[must_use]
pub fn conversations(day: &str, items: &[Conversation]) -> String {
    if items.is_empty() {
        return format!("no conversations on {day}");
    }
    let mut lines = vec![
        format!("# {day} — {} conversation(s)", items.len()),
        String::new(),
    ];
    lines.extend(items.iter().enumerate().map(|(i, c)| {
        format!(
            "{}. {}-{}  {:>3} turns  {}",
            i + 1,
            when(&c.start, "%H:%M"),
            when(&c.end, "%H:%M"),
            c.turn_count,
            c.preview
        )
    }));
    lines.join("\n")
}

/// One conversation, the best mic's version of each moment. `recall-cli show
/// <id>` shows a particular turn.
#[must_use]
pub fn conversation(title: &str, conv: &Conversation) -> String {
    let primary: Vec<&Turn> = conv.moments.iter().map(|m| &m.primary).collect();
    let mut header = format!("# {title}");
    if let Some(first) = primary.first() {
        header.push_str(&when(&first.start, "  (%a %d %b %Y %H:%M)"));
    }
    let mut lines = vec![header, String::new()];
    lines.extend(
        primary
            .iter()
            .map(|t| format!("[{}] {}: {}", when(&t.start, "%H:%M:%S"), who(t), t.text)),
    );
    lines.join("\n")
}
