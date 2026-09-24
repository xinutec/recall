import { Injectable, inject } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { Observable } from 'rxjs';

import {
  AssignResult,
  AssignSpanRequest,
  CaptureState,
  ConversationPage,
  CorrectRequest,
  CorrectResult,
  Ok,
  Session,
  SessionRenameRequest,
  SpeakerNames,
  VoiceNameRequest,
  VocabularyRequest,
} from './models';

/** The writes, and the reads a component pages through itself. Reactive reads
 * use `httpResource` in the component that owns the query. */
@Injectable({ providedIn: 'root' })
export class RecallApi {
  private readonly http = inject(HttpClient);

  /** Capture state. With `known` and `waitS` the server holds the request until
   * the state differs from `known`. */
  capture(known = '', waitS = 0): Observable<CaptureState> {
    const query = waitS > 0 ? `?wait=${waitS}&known=${encodeURIComponent(known)}` : '';
    return this.http.get<CaptureState>(`/api/capture${query}`);
  }

  pauseCapture(): Observable<CaptureState> {
    return this.http.post<CaptureState>('/api/capture/pause', {});
  }

  resumeCapture(): Observable<CaptureState> {
    return this.http.post<CaptureState>('/api/capture/resume', {});
  }

  /** Turns grouped into conversations at silences of `gap` seconds. Page back
   * with `before` (oldest start seen), forward with `after` (newest end seen). */
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

  correct(
    id: number,
    text: string,
    opts: Omit<CorrectRequest, 'id' | 'text'> = {},
  ): Observable<CorrectResult> {
    const body: CorrectRequest = { id, text, ...opts };
    return this.http.post<CorrectResult>('/api/correct', body);
  }

  /** Enrolled and assigned names. Runtime data, so no name is in the code. */
  speakers(): Observable<SpeakerNames> {
    return this.http.get<SpeakerNames>('/api/speakers');
  }

  /** Bias transcription towards a term from the next clip on. */
  addVocabularyTerm(term: string): Observable<CorrectResult> {
    const body: VocabularyRequest = { term };
    return this.http.post<CorrectResult>('/api/vocabulary', body);
  }

  deleteVocabularyTerm(id: number): Observable<Ok> {
    return this.http.delete<Ok>(`/api/vocabulary/${id}`);
  }

  /** Name (or clear) a session voice on all its turns; this enrols it. */
  nameSessionVoice(source: string, cluster: string, name: string | null): Observable<Ok> {
    const body: VoiceNameRequest = { cluster, name };
    return this.http.post<Ok>(`/api/sessions/${encodeURIComponent(source)}/voice`, body);
  }

  /** Re-assign a labelled fragment's voice (review/audit). */
  reassignCorrection(id: number, speaker: string): Observable<Ok> {
    return this.http.post<Ok>(`/api/correction/${id}/speaker`, { speaker });
  }

  /** Give a text span, across turns and with partial edges, to a speaker: the
   * server splits at the edges. Reassign, split and merge are all this. */
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

  /** Upload a recording as a new session. `start` is when it was recorded. */
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

  /** Queue a session for diarizing again. */
  rediarizeSession(source: string): Observable<Ok> {
    return this.http.post<Ok>(`/api/sessions/${encodeURIComponent(source)}/rediarize`, {});
  }
}
