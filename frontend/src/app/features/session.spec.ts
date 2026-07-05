import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { provideHttpClient } from '@angular/common/http';
import {
  HttpTestingController,
  provideHttpClientTesting,
} from '@angular/common/http/testing';
import { provideRouter } from '@angular/router';
import { MatSnackBar } from '@angular/material/snack-bar';
import { vi } from 'vitest';

import { Session } from './session';
import { ConversationPage, Transcript } from '../models';

function turn(o: Partial<Transcript>): Transcript {
  return {
    id: 0,
    start: '2026-01-15T10:33:00Z',
    end: '2026-01-15T10:33:05Z',
    text: 't',
    language: 'en',
    speaker: null,
    speakerConfirmed: false,
    speakerConfidence: null,
    confidence: null,
    loudness: null,
    model: 'diarized',
    tier: 'diarized',
    hidden: null,
    audioUrl: '/a',
    source: 'm',
    cluster: null,
    ...o,
  };
}

// One Pippijn turn: confirmed, in a cluster.
const said = (id: number, cluster: string, speaker?: string): Transcript =>
  turn({ id, cluster, speaker: speaker ?? null, speakerConfirmed: !!speaker });

function pageOf(turns: Transcript[]): ConversationPage {
  const start = turns[0]?.start ?? '2026-01-15T10:33:00Z';
  const end = turns.at(-1)?.end ?? start;
  return {
    items: turns.length
      ? [
          {
            start,
            end,
            turnCount: turns.length,
            speakers: [],
            preview: 'x',
            moments: [{ start, end, primary: turns, alternates: [], sources: ['m'] }],
          },
        ]
      : [],
    hasMore: false,
  };
}

async function setup(turns: Transcript[] = [], known: string[] = []) {
  TestBed.configureTestingModule({
    providers: [
      provideZonelessChangeDetection(),
      provideHttpClient(),
      provideHttpClientTesting(),
      provideRouter([]),
      { provide: MatSnackBar, useValue: { open: vi.fn() } },
    ],
  });
  const fixture = TestBed.createComponent(Session);
  fixture.componentRef.setInput('id', 'm');
  const ctrl = TestBed.inject(HttpTestingController);
  fixture.detectChanges(); // fire the three httpResources
  ctrl.match((r) => r.url.includes('/api/conversations')).forEach((r) => r.flush(pageOf(turns)));
  ctrl.match((r) => r.url.includes('/api/speakers')).forEach((r) => r.flush({ names: known }));
  ctrl.match((r) => r.url.includes('/voices')).forEach((r) => r.flush({ suggestions: {} }));
  await fixture.whenStable();
  fixture.detectChanges();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const c = fixture.componentInstance as any;
  return { fixture, c, ctrl };
}

describe('Session', () => {
  it('coalesces consecutive same-speaker turns into one run', async () => {
    const { c } = await setup([
      said(1, 'A', 'Pippijn'),
      said(2, 'A', 'Pippijn'), // same speaker → folds into the run above
      said(3, 'B', 'Dr. Adams'),
      said(4, 'A', 'Pippijn'),
    ]);
    const runs = c.runs();
    expect(runs.map((r: { speaker: string }) => r.speaker)).toEqual([
      'Pippijn',
      'Dr. Adams',
      'Pippijn',
    ]);
    expect(runs[0].turns.map((t: Transcript) => t.id)).toEqual([1, 2]);
  });

  it('groups voices by cluster, biggest first, named only on a majority', async () => {
    const { c } = await setup([
      said(1, 'A', 'Pippijn'),
      said(2, 'A', 'Pippijn'),
      said(3, 'A', 'Pippijn'), // 3/3 → named
      said(4, 'B', 'Dr. Adams'),
      said(5, 'B'), // only 1/2 confirmed → not a majority
    ]);
    const voices = c.voices();
    expect(voices.map((v: { cluster: string }) => v.cluster)).toEqual(['A', 'B']);
    expect(voices[0].name).toBe('Pippijn');
    expect(voices[0].turns).toBe(3);
    expect(voices[1].name).toBeNull(); // a single stray label doesn't name the voice
  });

  it('offers only this session’s named voices in the assign palette', async () => {
    const { c } = await setup(
      [said(1, 'A', 'Pippijn'), said(2, 'A', 'Pippijn'), said(3, 'B')],
      ['Pippijn', 'Alice', 'Carol'], // the household is known, but wasn't in this meeting
    );
    expect(c.palette()).toEqual(['Pippijn']);
  });

  it('lists a speaker present on even a single turn (a third person you added)', async () => {
    // One Sam turn lives in a cluster that's majority Pippijn — he must still appear in
    // the assign list, so the next marking can reuse him with one tap.
    const { c } = await setup([
      said(1, 'A', 'Pippijn'),
      said(2, 'A', 'Pippijn'),
      said(3, 'A', 'Sam'),
    ]);
    expect(c.palette()).toEqual(['Pippijn', 'Sam']);
  });

  it('gives each speaker a distinct colour', async () => {
    const { c } = await setup([said(1, 'A', 'Pippijn'), said(2, 'B', 'Dr. Adams')]);
    expect(c.colourFor('Pippijn')).toMatch(/^#/);
    expect(c.colourFor('Pippijn')).not.toBe(c.colourFor('Dr. Adams'));
  });

  it('a tap selects a turn, and tapping it again deselects', async () => {
    const { c } = await setup([said(1, 'A', 'Pippijn')]);
    c.selectTurn(1);
    expect(c.selected()).toBe(1);
    c.selectTurn(1);
    expect(c.selected()).toBeNull();
  });

  it('assignTurn posts to the per-turn endpoint, then clears the selection', async () => {
    const { c, ctrl } = await setup([said(1, 'A', 'Pippijn')]);
    c.selectTurn(1);
    c.assignTurn('Dr. Adams');
    const req = ctrl.expectOne('/api/turn/1/speaker');
    expect(req.request.body).toEqual({ name: 'Dr. Adams' });
    req.flush({ ok: true });
    expect(c.selected()).toBeNull();
  });

  it('Fix words opens the editor on the tapped turn, pre-filled', async () => {
    const { c } = await setup([
      turn({ id: 1, cluster: 'A', text: 'foracidinib' }),
      turn({ id: 2, cluster: 'A', text: 'other' }),
    ]);
    c.selectTurn(1);
    c.editText();
    expect(c.editing()).toBe(1);
    expect(c.editingText()).toBe('foracidinib');
  });

  it('saveEdit posts the corrected text for the edited turn, then closes', async () => {
    const { c, ctrl } = await setup([turn({ id: 1, cluster: 'A', text: 'foracidinib' })]);
    c.editing.set(1);
    c.saveEdit('vorasidenib');
    const req = ctrl.expectOne('/api/correct');
    expect(req.request.body).toEqual({ id: 1, text: 'vorasidenib' });
    req.flush({ newId: 99 });
    expect(c.editing()).toBeNull();
  });

  it('a blank edit is dropped (no correction posted)', async () => {
    const { c, ctrl } = await setup([turn({ id: 1, cluster: 'A', text: 'x' })]);
    c.editing.set(1);
    c.saveEdit('   ');
    ctrl.expectNone('/api/correct');
    expect(c.editing()).toBeNull();
  });

  it('togglePlay plays, pauses, then resumes the same clip (no reload)', async () => {
    // jsdom has no media playback; stub it so the toggle logic runs.
    const play = vi.spyOn(HTMLMediaElement.prototype, 'play').mockResolvedValue(undefined);
    const pause = vi.spyOn(HTMLMediaElement.prototype, 'pause').mockReturnValue(undefined);
    const { c } = await setup([said(1, 'A', 'Pippijn')]);
    const run = c.runs()[0];
    c.togglePlay(run); // play
    expect(c.playing()).toBe(`run:${run.key}`);
    c.togglePlay(run); // pause in place
    expect(c.playing()).toBeNull();
    c.togglePlay(run); // resume
    expect(c.playing()).toBe(`run:${run.key}`);
    // Played twice (start + resume), paused once — the pause kept its place.
    expect(play).toHaveBeenCalledTimes(2);
    expect(pause).toHaveBeenCalledTimes(1);
  });

  it('a voice sample stops when its button is tapped again', async () => {
    vi.spyOn(HTMLMediaElement.prototype, 'play').mockResolvedValue(undefined);
    vi.spyOn(HTMLMediaElement.prototype, 'pause').mockReturnValue(undefined);
    const { c } = await setup([said(1, 'A', 'Pippijn')]);
    c.toggleSample('A', '/api/audio/1');
    expect(c.playing()).toBe('voice:A');
    c.toggleSample('A', '/api/audio/1'); // tap again → stops
    expect(c.playing()).toBeNull();
  });

  it('nudgeTurn posts the boundary move, then replays the re-sliced clip', async () => {
    vi.spyOn(HTMLMediaElement.prototype, 'play').mockResolvedValue(undefined);
    const { c, ctrl } = await setup([said(1, 'A', 'Pippijn')]);
    c.selected.set(1);
    c.nudgeTurn('end', 0.1);
    const req = ctrl.expectOne('/api/turn/1/nudge');
    expect(req.request.body).toEqual({ edge: 'end', delta: 0.1 });
    req.flush({ ok: true });
    expect(c.playing()).toBe('turn'); // replays so you hear the trimmed clip
  });

  it('stops playback when the view is destroyed (audio must not outlive navigation)', async () => {
    vi.spyOn(HTMLMediaElement.prototype, 'play').mockResolvedValue(undefined);
    const pause = vi.spyOn(HTMLMediaElement.prototype, 'pause').mockReturnValue(undefined);
    const { fixture, c } = await setup([said(1, 'A', 'Pippijn')]);
    c.togglePlay(c.runs()[0]); // start playing a clip
    expect(c.playing()).not.toBeNull();
    const before = pause.mock.calls.length; // 0 — a fresh play doesn't pause
    fixture.destroy(); // navigate away from the session
    // Teardown must stop the shared <audio>; otherwise it keeps playing the clip.
    expect(pause.mock.calls.length).toBeGreaterThan(before);
  });

  it('maps a sub-phrase selection to turn id + char offsets, clamping the trailing space', async () => {
    const { fixture, c } = await setup([
      turn({ id: 7, cluster: 'A', speaker: 'Pippijn', speakerConfirmed: true, text: 'a list of errands' }),
    ]);
    const span = fixture.nativeElement.querySelector('span.t[data-id="7"]') as HTMLElement;
    const node = span.firstChild as Text; // "a list of errands " — the template adds a space
    const range = document.createRange();
    range.setStart(node, 0);
    range.setEnd(node, 'a list of errands'.length);
    expect(c.selectionToSpan(range)).toEqual({
      startTurn: 7,
      startChar: 0,
      endTurn: 7,
      endChar: 'a list of errands'.length,
    });
    // Selecting into the trailing space clamps to the real text length.
    range.setEnd(node, 'a list of errands'.length + 1);
    expect(c.selectionToSpan(range).endChar).toBe('a list of errands'.length);
  });

  it('assignSelectedSpan posts the selected span to the assign endpoint, then clears it', async () => {
    const { c, ctrl } = await setup([
      turn({ id: 7, cluster: 'A', speaker: 'Pippijn', speakerConfirmed: true, text: 'a list of errands' }),
    ]);
    c.span.set({ startTurn: 7, startChar: 0, endTurn: 7, endChar: 18 });
    c.assignSelectedSpan('Pippijn');
    const req = ctrl.expectOne('/api/sessions/m/assign');
    expect(req.request.body).toEqual({
      startTurn: 7,
      startChar: 0,
      endTurn: 7,
      endChar: 18,
      name: 'Pippijn',
    });
    req.flush({ touched: 1 });
    expect(c.span()).toBeNull();
  });

  it('assignSelectedSpan ignores a blank name (no request, selection kept)', async () => {
    const { c, ctrl } = await setup([
      turn({ id: 7, cluster: 'A', speaker: 'Pippijn', speakerConfirmed: true, text: 'a list of errands' }),
    ]);
    c.span.set({ startTurn: 7, startChar: 0, endTurn: 7, endChar: 18 });
    c.assignSelectedSpan('   ');
    ctrl.expectNone('/api/sessions/m/assign');
    expect(c.span()).not.toBeNull();
  });

  it('assignSelectedSpan ignores repeated taps while a request is in flight', async () => {
    const { c, ctrl } = await setup([
      turn({ id: 7, cluster: 'A', speaker: 'Pippijn', speakerConfirmed: true, text: 'a list of errands' }),
    ]);
    c.span.set({ startTurn: 7, startChar: 0, endTurn: 7, endChar: 18 });
    c.assignSelectedSpan('Dr'); // first tap fires
    c.assignSelectedSpan('Dr'); // impatient repeats while in flight…
    c.assignSelectedSpan('Dr');
    const reqs = ctrl.match('/api/sessions/m/assign');
    expect(reqs.length).toBe(1); // …only one split request is sent
    reqs[0].flush({ touched: 1 });
  });

  it('plays a joined bubble as one full span, not just the first turn', async () => {
    vi.spyOn(HTMLMediaElement.prototype, 'play').mockResolvedValue(undefined);
    const { c } = await setup([said(1, 'A', 'Pippijn'), said(2, 'A', 'Pippijn')]);
    const run = c.runs()[0];
    expect(run.turns.length).toBe(2); // two same-speaker turns coalesce into one bubble
    c.togglePlay(run);
    expect(c.audio.src).toContain('/api/audio-span?from_id=1&to_id=2');
  });
});
