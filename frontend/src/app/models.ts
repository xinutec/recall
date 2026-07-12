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
}

export interface Status {
  readonly audioSegments: number;
  readonly transcripts: number;
  readonly pending: number;
  readonly corrections: number;
  readonly sources: readonly string[];
}

export interface Ok {
  readonly ok: boolean;
}

export interface QuietSpan {
  readonly source: string;
  readonly start: string;
  readonly end: string;
  readonly durationS: number;
  readonly audioIds: readonly number[];
  readonly soundSeconds: number;
  readonly loudestDb: number | null;
  readonly marginDb: number | null;
  readonly silent: boolean;
  readonly structure: number | null;
}

export interface QuietSpanList {
  readonly items: readonly QuietSpan[];
}

export interface QuietScan {
  readonly running: boolean;
  readonly measured: number;
  readonly total: number;
  readonly analysed: number;
  readonly toAnalyse: number;
}

export interface QuietDeleted {
  readonly deleted: number;
  readonly freedBytes: number;
}

export interface EnvelopeSegment {
  readonly audioId: number;
  readonly start: string;
  readonly end: string;
  readonly meanDb: number | null;
}

export interface SoundEvent {
  readonly start: string;
  readonly end: string;
  readonly peakDb: number;
}

export interface Envelope {
  readonly start: string;
  readonly end: string;
  readonly bucketS: number;
  readonly thresholdDb: number;
  readonly points: readonly (number | null)[];
  readonly segments: readonly EnvelopeSegment[];
  readonly events: readonly SoundEvent[];
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

export interface DaySummary {
  readonly day: string;
  readonly text: string;
  readonly model: string;
}

export interface DaySummaryList {
  readonly items: readonly DaySummary[];
}

export interface HouseholdContext {
  readonly text: string;
}

export interface TodaySummary {
  readonly day: string;
  readonly text: string | null;
  readonly generatedAt: string | null;
  readonly upToDate: boolean;
  readonly pending: boolean;
}

export interface AskAnswer {
  readonly answer: string | null;
  readonly sources: readonly Transcript[];
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

export interface AbCompareScore {
  readonly correctionId: number;
  readonly truth: string;
  readonly textA: string;
  readonly textB: string;
  readonly werA: number;
  readonly werB: number;
  readonly audioUrl: string;
}

export interface AbCompareSegmentDiff {
  readonly audioId: number;
  readonly start: string;
  readonly changed: boolean;
  readonly textA: string;
  readonly textB: string;
}

export interface AbCompareRunSummary {
  readonly id: number;
  readonly source: string;
  readonly modelA: string;
  readonly modelB: string;
  readonly baseModel: string;
  readonly status: 'queued' | 'running' | 'done' | 'error';
  readonly created: string;
  readonly meanWerA: number | null;
  readonly meanWerB: number | null;
  readonly nCorrections: number | null;
  readonly nSegments: number | null;
  readonly nChanged: number | null;
  readonly error: string | null;
}

export interface AbCompareRunList {
  readonly items: readonly AbCompareRunSummary[];
}

export interface AbCompareRun {
  readonly summary: AbCompareRunSummary;
  readonly scores: readonly AbCompareScore[];
  readonly segmentDiffs: readonly AbCompareSegmentDiff[];
}

// ---- request bodies (POST payloads) ----

export interface ClientLogRequest {
  readonly level?: string;
  readonly message: string;
  readonly stack?: string | null;
  readonly url?: string | null;
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

export interface AbCompareStartRequest {
  readonly source: string;
  readonly from?: string | null;
  readonly to?: string | null;
  readonly modelA?: string | null;
  readonly modelB?: string | null;
  readonly baseModel?: string | null;
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

export interface AskRequest {
  readonly question: string;
}

export interface QuietDeleteRequest {
  readonly audioIds: readonly number[];
}

export interface VocabularyRequest {
  readonly term: string;
}

export interface SessionRenameRequest {
  readonly title: string;
}

export interface ContextRequest {
  readonly text: string;
}
