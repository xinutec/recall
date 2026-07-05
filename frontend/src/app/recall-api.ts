import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { Observable } from 'rxjs';

import {
  AbCompareStartRequest,
  Around,
  AskAnswer,
  AskRequest,
  AssignResult,
  AssignSpanRequest,
  CaptureState,
  ContextRequest,
  ConversationPage,
  CorrectRequest,
  CorrectResult,
  NudgeRequest,
  Ok,
  RefineRequest,
  Session,
  SessionRenameRequest,
  SpeakerNames,
  SplitFragment,
  TurnSpeakerRequest,
  VoiceNameRequest,
  SplitResult,
  Suggest,
  TrainQueue,
  TranscriptList,
  VocabularyRequest,
} from './models';

/**
 * Thin client over the FastAPI backend (src/recall/api.py).
 *
 * Reactive reads (status / search / review) are done with `httpResource` in the
 * components that own their query signals; this service holds the mutations
 * (correct / suggest / upload) and the audio-clip URL helper.
 */
@Injectable({ providedIn: 'root' })
export class RecallApi {
  private readonly http = inject(HttpClient);

  /** Whether the always-on capture is recording (or paused, with auto-resume time). */
  capture(): Observable<CaptureState> {
    return this.http.get<CaptureState>('/api/capture');
  }

  pauseCapture(): Observable<CaptureState> {
    return this.http.post<CaptureState>('/api/capture/pause', {});
  }

  resumeCapture(): Observable<CaptureState> {
    return this.http.post<CaptureState>('/api/capture/resume', {});
  }

  /** Recent turns grouped into conversations by silence gaps (`gap` seconds).
   * Page back with `before` (oldest start seen) or forward with `after` (newest
   * end seen). */
  conversations(
    limit: number,
    before?: string,
    after?: string,
    gap?: number,
  ): Observable<ConversationPage> {
    const params = new URLSearchParams({ limit: String(limit) });
    if (before) {
      params.set('before', before);
    }
    if (after) {
      params.set('after', after);
    }
    if (gap !== undefined) {
      params.set('gap', String(gap));
    }
    return this.http.get<ConversationPage>(`/api/conversations?${params.toString()}`);
  }

  trainQueue(limit = 40, since?: string, until?: string, order?: string): Observable<TrainQueue> {
    const params = new URLSearchParams({ limit: String(limit) });
    if (since) {
      params.set('since', since);
    }
    if (until) {
      params.set('until', until);
    }
    if (order) {
      params.set('order', order);
    }
    return this.http.get<TrainQueue>(`/api/train?${params.toString()}`);
  }

  unintelligible(id: number): Observable<Ok> {
    return this.http.post<Ok>('/api/unintelligible', { id });
  }

  correct(
    id: number,
    text: string,
    opts: Omit<CorrectRequest, 'id' | 'text'> = {},
  ): Observable<CorrectResult> {
    const body: CorrectRequest = { id, text, ...opts };
    return this.http.post<CorrectResult>('/api/correct', body);
  }

  around(id: number): Observable<Around> {
    return this.http.get<Around>(`/api/around/${id}`);
  }

  /** Specific turns by id (comma-separated) — backs labelling one turn in Train. */
  transcripts(ids: string): Observable<TranscriptList> {
    return this.http.get<TranscriptList>(`/api/transcripts?ids=${encodeURIComponent(ids)}`);
  }

  /** Best-matching enrolled name for a turn (or null) — the "sounds like X" hint. */
  suggest(id: number): Observable<Suggest> {
    return this.http.get<Suggest>(`/api/suggest/${id}`);
  }

  /** The household roster — enrolled voices + assigned labels — for the quick-pick
   * speaker chips. Names live in runtime enrolment data, never hard-coded here. */
  speakers(): Observable<SpeakerNames> {
    return this.http.get<SpeakerNames>('/api/speakers');
  }

  /** Add a term to the household vocabulary (biases the ASR from the next
   * transcription — names, places, medical terms). */
  addVocabularyTerm(term: string): Observable<CorrectResult> {
    const body: VocabularyRequest = { term };
    return this.http.post<CorrectResult>('/api/vocabulary', body);
  }

  deleteVocabularyTerm(id: number): Observable<Ok> {
    return this.http.delete<Ok>(`/api/vocabulary/${id}`);
  }

  /** Ask the archive a question — grounded retrieval + local generation. Slow
   * (seconds; the first call also loads the model), so callers show progress. */
  ask(question: string): Observable<AskAnswer> {
    const body: AskRequest = { question };
    return this.http.post<AskAnswer>('/api/ask', body);
  }

  /** Assign one turn to a person (display label only; `name` may be brand-new). */
  setTurnSpeaker(id: number, name: string): Observable<Ok> {
    const body: TurnSpeakerRequest = { name };
    return this.http.post<Ok>(`/api/turn/${id}/speaker`, body);
  }

  /** Name (or clear) a diarization voice across a whole session — this enrols it. */
  nameSessionVoice(source: string, cluster: string, name: string | null): Observable<Ok> {
    const body: VoiceNameRequest = { cluster, name };
    return this.http.post<Ok>(`/api/sessions/${encodeURIComponent(source)}/voice`, body);
  }

  /** Re-assign a labelled fragment's voice (review/audit). */
  reassignCorrection(id: number, speaker: string): Observable<Ok> {
    return this.http.post<Ok>(`/api/correction/${id}/speaker`, { speaker });
  }

  /** Assign a text span (across turns, with partial edges) to a speaker — the one
   * gesture behind reassign / split / merge. Splits at the edges and relabels. */
  assignSpan(source: string, body: AssignSpanRequest): Observable<AssignResult> {
    return this.http.post<AssignResult>(
      `/api/sessions/${encodeURIComponent(source)}/assign`,
      body,
    );
  }

  /** Soft-remove a bad label from the corpus. */
  hideCorrection(id: number): Observable<Ok> {
    return this.http.post<Ok>(`/api/correction/${id}/hide`, {});
  }

  /** Move one boundary of a label (fix a too-tight/too-loose cut). */
  nudgeCorrection(id: number, edge: 'start' | 'end', delta: number): Observable<Ok> {
    const body: NudgeRequest = { edge, delta };
    return this.http.post<Ok>(`/api/correction/${id}/nudge`, body);
  }

  /** Move one edge of a turn by ear (hand-tune a split boundary the aligner got wrong). */
  nudgeTurn(id: number, edge: 'start' | 'end', delta: number): Observable<Ok> {
    const body: NudgeRequest = { edge, delta };
    return this.http.post<Ok>(`/api/turn/${id}/nudge`, body);
  }

  /** Queue an on-demand diarize-refine of [start, end) of a recording (the idle daemon
   * runs it). `start`/`end` are ISO 8601 strings. */
  refineRange(source: string, start: string, end: string): Observable<Ok> {
    const body: RefineRequest = { source, start, end };
    return this.http.post<Ok>('/api/refine', body);
  }

  split(id: number, fragments: SplitFragment[]): Observable<SplitResult> {
    return this.http.post<SplitResult>('/api/split', { id, fragments });
  }

  unhide(id: number): Observable<Ok> {
    return this.http.post<Ok>('/api/unhide', { id });
  }

  /** Queue a non-destructive A/B comparison; the daemon runs it. Returns its id. */
  startAbCompare(body: AbCompareStartRequest): Observable<CorrectResult> {
    return this.http.post<CorrectResult>('/api/ab-compare', body);
  }

  /** Upload a conversation recording (e.g. a hospital appointment) as a new session.
   * `start` is the recording's local start (ISO 8601); `title` optional. The file's
   * container is kept as-is — the backend probes the real content. */
  createSession(file: File, title: string, start: string): Observable<Session> {
    const form = new FormData();
    form.append('audio', file, file.name);
    if (title) form.append('title', title);
    if (start) form.append('start', start);
    return this.http.post<Session>('/api/sessions', form);
  }

  renameSession(source: string, title: string): Observable<Ok> {
    const body: SessionRenameRequest = { title };
    return this.http.patch<Ok>(`/api/sessions/${encodeURIComponent(source)}`, body);
  }

  deleteSession(source: string): Observable<Ok> {
    return this.http.delete<Ok>(`/api/sessions/${encodeURIComponent(source)}`);
  }

  /** Re-derive who-said-what for a whole session (idle-gated; never inline). */
  rediarizeSession(source: string): Observable<Ok> {
    return this.http.post<Ok>(`/api/sessions/${encodeURIComponent(source)}/rediarize`, {});
  }

  /** Replace the household context (background facts given to the LLM). */
  setContext(text: string): Observable<Ok> {
    const body: ContextRequest = { text };
    return this.http.put<Ok>('/api/context', body);
  }
}
