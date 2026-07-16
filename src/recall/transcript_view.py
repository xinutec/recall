"""Render the session list, or one session's transcript, as plain text or JSON.

For reading a recorded call from the CLI or handing it to a reviewer (e.g. another
agent). Pure formatting — no database — so it's unit-tested directly.
"""

from __future__ import annotations

import json
from datetime import datetime

from recall.schemas import TranscriptBubbleOut, TranscriptExportOut
from recall.store import SessionSummary, TranscriptSegment

# A day-conversation summary row: (number, start, end, turn_count, preview).
ConversationRow = tuple[int, datetime, datetime, int, str]


def _duration(start_iso: str, end_iso: str) -> str:
    span = datetime.fromisoformat(end_iso) - datetime.fromisoformat(start_iso)
    hours, rest = divmod(max(int(span.total_seconds()), 0), 3600)
    minutes, seconds = divmod(rest, 60)
    if hours:
        return f"{hours}h{minutes:02d}m"
    if minutes:
        return f"{minutes}m{seconds:02d}s"
    return f"{seconds}s"


def format_sessions(summaries: list[SessionSummary], *, as_json: bool = False) -> str:
    """A reviewable index of recorded sessions: id, when, how long, turn count, who."""
    if as_json:
        return json.dumps(
            [
                {
                    "id": sid,
                    "name": name,
                    "start": start,
                    "end": end,
                    "turns": turns,
                    "speakers": speakers,
                }
                for sid, name, start, end, turns, speakers in summaries
            ],
            indent=2,
        )
    if not summaries:
        return "no sessions recorded"
    lines = []
    for sid, _name, start, end, turns, speakers in summaries:
        when = datetime.fromisoformat(start).astimezone().strftime("%a %d %b %Y %H:%M")
        lines.append(
            f"{sid}  {when}  {_duration(start, end):>7}  {turns:>4} turns  "
            f"{speakers or 'unknown'}"
        )
    return "\n".join(lines)


def format_conversations(
    day: str, rows: list[ConversationRow], *, as_json: bool = False
) -> str:
    """A day's conversations (continuous capture, split by silence), numbered so each
    can be dumped on demand. Each row: (n, start, end, turn_count, preview)."""
    if as_json:
        return json.dumps(
            [
                {
                    "n": n,
                    "start": start.isoformat(),
                    "end": end.isoformat(),
                    "turns": count,
                    "preview": preview,
                }
                for n, start, end, count, preview in rows
            ],
            indent=2,
        )
    if not rows:
        return f"no conversations on {day}"
    lines = [f"# {day} — {len(rows)} conversation(s)", ""]
    for n, start, end, count, preview in rows:
        span = f"{start.astimezone():%H:%M}-{end.astimezone():%H:%M}"
        lines.append(f"{n}. {span}  {count:>3} turns  {preview}")
    return "\n".join(lines)


def attribution(turn: TranscriptSegment) -> str:
    """Who said a turn, for a search hit. A human-confirmed name is authoritative;
    otherwise the voiceprint guess with its match strength as a hint
    (e.g. "Pippijn ~76%"); otherwise the bare diarization voice; otherwise "unknown".
    Mirrors the web timeline's who-column, where most hits are unconfirmed machine
    turns and the guess is the only signal — so search surfaces it, unlike `_who`
    (which a read-through transcript keeps to confirmed-or-cluster, no speculation)."""
    if turn.speaker_label:
        return turn.speaker_label
    if turn.speaker_guess:
        score = turn.speaker_score
        strength = f" ~{round(score * 100)}%" if score is not None else ""
        return f"{turn.speaker_guess}{strength}"
    return turn.speaker_cluster or "unknown"


def _who(turn: TranscriptSegment) -> str:
    # A confirmed name wins; otherwise the diarization voice (so distinct unnamed
    # speakers stay distinguishable); 'unknown' only if there's neither.
    return turn.speaker_label or turn.speaker_cluster or "unknown"


def clean_transcript(
    source_id: str, turns: list[TranscriptSegment]
) -> TranscriptExportOut:
    """A session's clean, finalised transcript: consecutive same-speaker turns merged
    into one bubble, each with its local start time and display speaker (a confirmed
    name, else the diarization voice). The current/corrected state only; deterministic,
    so re-export of unchanged data is byte-identical. Shared by the CLI and the API.
    """
    bubbles: list[TranscriptBubbleOut] = []
    speakers: list[str] = []
    for turn in turns:
        who = _who(turn)
        if bubbles and bubbles[-1]["speaker"] == who:
            bubbles[-1]["text"] = f"{bubbles[-1]['text']} {turn.text}".strip()
        else:
            bubbles.append(
                {
                    "start": turn.start.astimezone().isoformat(),
                    "speaker": who,
                    "text": turn.text,
                }
            )
        label = turn.speaker_label
        if label and not label.startswith("SPEAKER") and label not in speakers:
            speakers.append(label)
    return {
        "session": source_id,
        "date": bubbles[0]["start"] if bubbles else None,
        "speakers": speakers,
        "turns": bubbles,
    }


def format_transcript(
    source_id: str, turns: list[TranscriptSegment], *, as_json: bool = False
) -> str:
    """One session as a speaker-attributed transcript — what a reviewer reads."""
    if as_json:
        return json.dumps(clean_transcript(source_id, turns), indent=2)
    # Times stored UTC; show them in the local wall-clock the call actually happened on.
    header = f"# {source_id}"
    if turns:
        header += turns[0].start.astimezone().strftime("  (%a %d %b %Y %H:%M)")
    lines = [header, ""]
    lines.extend(
        f"[{turn.start.astimezone():%H:%M:%S}] {_who(turn)}: {turn.text}"
        for turn in turns
    )
    return "\n".join(lines)


# Audibility bands from measured loudness (mirror the labeling UI, train.ts).
_CLEAR_LOUDNESS = 0.05
_QUIET_LOUDNESS = 0.01


def _clarity(loudness: float | None) -> str:
    """The audibility band shown in the labeling UI (matches train.ts thresholds)."""
    if loudness is None:
        return "unmeasured"
    if loudness >= _CLEAR_LOUDNESS:
        return "clear"
    if loudness >= _QUIET_LOUDNESS:
        return "quiet"
    return "faint"


def _who_detail(turn: TranscriptSegment) -> str:
    """Attribution for the diagnostic view, tagged with how it was decided."""
    if turn.speaker_label and not turn.speaker_label.startswith("SPEAKER_"):
        return f"{turn.speaker_label} (confirmed)"
    if turn.speaker_guess:
        score = turn.speaker_score
        pct = f" ~{round(score * 100)}%" if score is not None else ""
        return f"{turn.speaker_guess}{pct} (guess)"
    return "unknown"


def format_turn_details(turns: list[TranscriptSegment]) -> str:
    """A diagnostic dump of specific turns — every field that explains a turn's state:
    confidence, loudness/clarity, attribution, provenance, and whether it's still
    current. For inspecting one by id rather than reading a whole session; this is the
    raw truth the timeline/search only summarise (e.g. a high `conf` on `faint` audio
    is the decoder's self-confidence, not a clarity signal)."""
    blocks: list[str] = []
    for t in turns:
        duration = (t.end - t.start).total_seconds()
        if t.hidden_reason:
            status = f"hidden: {t.hidden_reason}"
        elif t.superseded_by is not None:
            status = f"superseded by #{t.superseded_by}"
        else:
            status = "visible"
        conf = "—" if t.asr_confidence is None else f"{t.asr_confidence:.2f}"
        loud = "—" if t.loudness is None else f"{t.loudness:.5f}"
        blocks.append(
            "\n".join(
                [
                    f"#{t.id}  {t.start.astimezone():%a %d %b %Y %H:%M:%S}"
                    f"-{t.end.astimezone():%H:%M:%S}  ({duration:.1f}s)"
                    f"  [{t.language or '?'}]  src={t.source_id or '—'}",
                    f"  who      : {_who_detail(t)}   voice={t.speaker_cluster or '—'}",
                    f"  conf     : {conf}   loudness: {loud} ({_clarity(t.loudness)})",
                    f"  model    : {t.asr_model}",
                    f"  provenance: {t.provenance or '—'}",
                    f"  status   : {status}",
                    f"  text     : {t.text}",
                ]
            )
        )
    return "\n\n".join(blocks)
