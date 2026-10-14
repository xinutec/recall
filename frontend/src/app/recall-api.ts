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
  ExcludeRequest,
  LineRequest,
  Ok,
  Session,
  SessionLanguageRequest,
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

  /** Turns grouped into conversations at silences. Page back with `before`
   * (oldest start seen), forward with `after` (newest end seen). */
  conversations(limit: number, before?: string, after?: string): Observable<ConversationPage> {
    const params = new URLSearchParams({ limit: String(limit) });
    if (before) {
      params.set('before', before);
    }
    if (after) {
      params.set('after', after);
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

  /** Take back a correction or a check of line `id`, which shows as the
   * machine wrote it again. */
  undoCorrection(id: number): Observable<Ok> {
    const body: LineRequest = { id };
    return this.http.post<Ok>('/api/correct/undo', body);
  }

  /** Nobody spoke: hide the line, and file that its words were invented. */
  noSpeech(id: number): Observable<Ok> {
    const body: LineRequest = { id };
    return this.http.post<Ok>('/api/no-speech', body);
  }

  /** Take back a nobody spoke: the line shows again. */
  undoNoSpeech(id: number): Observable<Ok> {
    const body: LineRequest = { id };
    return this.http.post<Ok>('/api/no-speech/undo', body);
  }

  /** Leave out the stretch from line `from` to line `to`, as if recording
   * had paused: strangers talking. The words are kept. */
  exclude(from: number, to: number): Observable<Ok> {
    const body: ExcludeRequest = { from, to };
    return this.http.post<Ok>('/api/exclude', body);
  }

  /** Take back an excluded stretch, by any of its lines. */
  undoExclude(id: number): Observable<Ok> {
    const body: LineRequest = { id };
    return this.http.post<Ok>('/api/exclude/undo', body);
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
    return this.http.post<AssignResult>(`/api/sessions/${encodeURIComponent(source)}/assign`, body);
  }

  /** Take a bad label out of the training set; the line itself stays. */
  hideCorrection(id: number): Observable<Ok> {
    return this.http.post<Ok>(`/api/correction/${id}/hide`, {});
  }

  /**
   * Upload a recording as a new session, titled by its start. `start` is when it
   * was recorded;
   * `language` ('nl', 'en') pins its transcription, empty leaves it to the model.
   */
  createSession(file: File, start: string, language = ''): Observable<Session> {
    const form = new FormData();
    form.append('audio', file, file.name);
    if (start) form.append('start', start);
    if (language) form.append('language', language);
    return this.http.post<Session>('/api/sessions', form);
  }

  /** Pin a session's language (null: the model's guess) and transcribe it again. */
  setSessionLanguage(source: string, language: string | null): Observable<Ok> {
    const body: SessionLanguageRequest = { language };
    return this.http.post<Ok>(`/api/sessions/${encodeURIComponent(source)}/language`, body);
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
