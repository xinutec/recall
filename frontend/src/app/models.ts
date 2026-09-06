/** The FastAPI backend's API shapes (src/recall/api.py): responses first,
 *  then request bodies.
 *
 *  GENERATED from src/recall/schemas.py + src/recall/api_models.py by
 *  scripts/gen_models.py — do not edit. Run `scripts/gen_models.py --write`
 *  after changing a backend shape; the verify gate fails if this file is
 *  stale. */

export interface Transcript {
  readonly id: number;
  readonly start: string;
  readonly end: string;
  readonly text: string;
  readonly language: string | null;
  readonly speaker: string | null;
  readonly speakerConfirmed: boolean;
  readonly speakerConfidence: number | null;
  readonly confidence: number | null;
  readonly loudness: number | null;
  readonly model: string;
  readonly tier: 'live' | 'transcribed' | 'diarized' | 'corrected';
  readonly hidden: string | null;
  readonly audioUrl: string;
  readonly source: string | null;
  readonly cluster: string | null;
}

export interface DeviceOutbox {
  readonly device: string;
  readonly queued: number;
  readonly oldestQueuedAt: string | null;
  readonly failing: number;
  readonly reason: string | null;
  readonly at: string;
}

export interface DeviceOutboxList {
  readonly items: readonly DeviceOutbox[];
}

export interface DeviceHeartbeat {
  readonly device: string;
  readonly app: string;
  readonly version: string;
  readonly startedAt: string | null;
  readonly streaming: boolean;
  readonly charging: boolean | null;
  readonly micOk: boolean | null;
  readonly viaLan: boolean | null;
  readonly at: string;
}

export interface DeviceHeartbeatList {
  readonly items: readonly DeviceHeartbeat[];
}

export interface Session {
  readonly id: string;
  readonly title: string;
  readonly start: string;
  readonly end: string;
  readonly turnCount: number;
  readonly speakers: readonly string[];
}

export interface SessionList {
  readonly items: readonly Session[];
}

export interface TranscriptBubble {
  readonly start: string;
  readonly speaker: string;
  readonly text: string;
}

export interface TranscriptExport {
  readonly session: string;
  readonly date: string | null;
  readonly speakers: readonly string[];
  readonly turns: readonly TranscriptBubble[];
}

export interface Moment {
  readonly start: string;
  readonly end: string;
  readonly primary: readonly Transcript[];
  readonly alternates: readonly Transcript[];
  readonly sources: readonly string[];
}

export interface Conversation {
  readonly start: string;
  readonly end: string;
  readonly turnCount: number;
  readonly speakers: readonly string[];
  readonly preview: string;
  readonly moments: readonly Moment[];
}

export interface Label {
  readonly id: number;
  readonly text: string;
  readonly speaker: string | null;
  readonly language: string | null;
  readonly start: string;
  readonly audioUrl: string;
}

export interface CaptureState {
  readonly running: boolean;
  readonly pausedUntil: string | null;
  readonly desiredRunning: boolean;
  readonly desiredPausedUntil: string | null;
  readonly settled: boolean;
  readonly micReachable: boolean;
  readonly stateToken: string;
}

export interface Ok {
  readonly ok: boolean;
}

export interface SoundEvent {
  readonly start: string;
  readonly end: string;
  readonly peakDb: number;
}

export interface SpeakerNames {
  readonly names: readonly string[];
}

export interface AssignResult {
  readonly touched: number;
}

export interface VoiceSuggestions {
  readonly suggestions: Record<string, string>;
}

export interface VocabularyTerm {
  readonly id: number;
  readonly term: string;
}

export interface VocabularyList {
  readonly items: readonly VocabularyTerm[];
}

export interface TranscriptList {
  readonly items: readonly Transcript[];
}

export interface TimelinePage {
  readonly items: readonly Transcript[];
  readonly hasMore: boolean;
}

export interface ConversationPage {
  readonly items: readonly Conversation[];
  readonly hasMore: boolean;
}

export interface TrainQueue {
  readonly items: readonly Transcript[];
  readonly corrections: number;
  readonly bySpeaker: Record<string, number>;
}

export interface LabelList {
  readonly items: readonly Label[];
  readonly bySpeaker: Record<string, number>;
}

export interface CorrectResult {
  readonly newId: number;
}

export interface SplitResult {
  readonly newIds: readonly number[];
}

export interface Around {
  readonly before: readonly Transcript[];
  readonly after: readonly Transcript[];
}

export interface Suggest {
  readonly speaker: string | null;
}

// ---- request bodies (POST payloads) ----

export interface ClientLogRequest {
  readonly level?: string;
  readonly message: string;
  readonly stack?: string | null;
  readonly url?: string | null;
}

export interface TelemetryEvent {
  readonly kind: string;
  readonly path: string;
  readonly label?: string | null;
  readonly at?: number;
}

export interface DeviceOutboxRequest {
  readonly device: string;
  readonly queued?: number;
  readonly oldestQueuedAt?: string | null;
  readonly failing?: number;
  readonly reason?: string | null;
}

export interface DeviceHeartbeatRequest {
  readonly device: string;
  readonly app?: string;
  readonly version?: string;
  readonly startedAt?: string | null;
  readonly streaming?: boolean;
  readonly charging?: boolean | null;
  readonly micOk?: boolean | null;
  readonly viaLan?: boolean | null;
}

export interface CorrectRequest {
  readonly id: number;
  readonly text: string;
  readonly speaker?: string | null;
  readonly start?: string | null;
  readonly end?: string | null;
  readonly language?: string | null;
}

export interface VoiceNameRequest {
  readonly cluster: string;
  readonly name?: string | null;
}

export interface TurnSpeakerRequest {
  readonly name?: string | null;
}

export interface AssignSpanRequest {
  readonly startTurn: number;
  readonly startChar: number;
  readonly endTurn: number;
  readonly endChar: number;
  readonly name: string;
}

export interface UnintelligibleRequest {
  readonly id: number;
}

export interface UnhideRequest {
  readonly id: number;
}

export interface NudgeRequest {
  readonly edge: string;
  readonly delta: number;
}

export interface RefineRequest {
  readonly source: string;
  readonly start: string;
  readonly end: string;
}

export interface ReassignRequest {
  readonly speaker: string;
}

export interface SplitFragment {
  readonly start: string;
  readonly end: string;
  readonly text: string;
  readonly speaker?: string | null;
}

export interface SplitRequest {
  readonly id: number;
  readonly fragments: readonly SplitFragment[];
}

export interface VocabularyRequest {
  readonly term: string;
}

export interface SessionRenameRequest {
  readonly title: string;
}
